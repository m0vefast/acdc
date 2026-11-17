# Asciidoctor -> ASG adapter for the acdc/asciidoctor AST differential
# (src/proptests/asciidoctor_diff.rs).
#
# One process for many documents: each stdin line is {"src": "..."}, each stdout
# line {"asg": {...}, "warnings": [...]} or {"error": "..."}.
#
# Blocks are read from Asciidoctor's document tree. Asciidoctor has no inline
# tree: inline syntax is resolved by substitutions that emit HTML, so inlines are
# read back from that HTML (the approach of the AsciiDoc TCK's former
# Asciidoctor.js sample adapter). Only the node shapes the differential compares
# are emitted; locations are omitted.
require 'asciidoctor'
require 'json'
require 'logger'
require 'cgi'

$stdout.sync = true

class CapturingLogger < Logger
  attr_reader :messages

  def initialize
    super(IO::NULL)
    @messages = []
  end

  def add(severity, message = nil, progname = nil)
    return true if severity < Logger::WARN
    msg = message || (block_given? ? yield : progname)
    @messages << (msg.respond_to?(:text) ? msg.text : msg.to_s)
    true
  end
end

module Inlines
  TAG = %r{<(/?)([a-z0-9]+)((?:\s+[a-z-]+(?:="[^"]*")?)*)\s*/?>}i
  SPANS = { 'strong' => 'strong', 'em' => 'emphasis', 'code' => 'code', 'mark' => 'mark',
            'sub' => 'subscript', 'sup' => 'superscript' }.freeze

  def self.parse(html)
    root = { children: [] }
    stack = [root]
    pos = 0
    html.scan(TAG) do
      m = Regexp.last_match
      text(stack.last, html[pos...m.begin(0)])
      pos = m.end(0)
      closing, tag, attrs = m[1] == '/', m[2].downcase, m[3]
      macro = MACROS[[tag, attr(attrs, 'class')&.split&.first]]
      in_macro = stack.any? { |n| n[:macro] }
      # A font callout is `<i class="conum">` followed by its `<b>(N)</b>` glyph.
      macro = 'glyph' if !macro && tag == 'b' && !closing && stack.last[:children].last&.dig(:macro) == 'callout'
      if closing && stack.size > 1 && stack.last[:tag] == tag
        stack.pop
      elsif closing || !(macro || SPANS.key?(tag) || %w[a span br img].include?(tag))
        # Markup Asciidoctor does not draw is raw passthrough output: its text.
        text(stack.last, m[0]) unless in_macro || EMITTED.include?(tag)
        # A font icon names itself only in its glyph's class.
        stack.last[:target] ||= attr(attrs, 'class')[/\bfa-(\S+)/, 1] if !closing && tag == 'i' && stack.last[:macro] == 'icon'
      elsif tag == 'br'
        stack.last[:children] << { 'name' => 'break' }
      elsif tag == 'img'
        # The image belongs to its macro even when a link wraps it.
        owner = stack.reverse.find { |n| n[:macro] } || stack.last
        owner[:target] ||= CGI.unescapeHTML(attr(attrs, 'src') || '')
      elsif macro || SPANS.key?(tag) || tag == 'a' || tag == 'span'
        node = { tag: tag, attrs: attrs, macro: macro, children: [] }
        stack.last[:children] << node
        stack << node
      end
    end
    text(stack.last, html[pos..])
    root[:children].map { |c| to_asg(c) }.compact
  end

  # Inline macros Asciidoctor draws as markup: the tag and class that mark each.
  MACROS = { %w[sup footnote] => 'footnote', %w[sup footnoteref] => 'footnote', %w[span keyseq] => 'keyboard', %w[kbd] + [nil] => 'keyboard',
             %w[b button] => 'button', %w[span menuseq] => 'menu', %w[span image] => 'image',
             %w[span icon] => 'icon', %w[b conum] => 'callout', %w[i conum] => 'callout' }.freeze

  # Tags Asciidoctor emits beside the ones read above (font icons, callout glyphs).
  EMITTED = %w[i].freeze

  # `\$…\$` (asciimath) and `\(…\)` (latexmath) are inline stem.
  STEM = /\\\$.+?\\\$|\\\(.+?\\\)/m

  def self.text(parent, raw)
    return if raw.nil? || raw.empty?
    CGI.unescapeHTML(raw).split(/(#{STEM})/o).each do |piece|
      next if piece.empty?
      parent[:children] << (piece.match?(/\A#{STEM}\z/o) ? { 'name' => 'stem' } : { 'name' => 'text', 'value' => piece })
    end
  end

  def self.plain(node)
    node[:children].map { |c| c.key?(:tag) ? plain(c) : c['value'].to_s }.join
  end

  def self.attr(attrs, name)
    attrs[/\b#{name}="([^"]*)"/, 1]
  end

  def self.to_asg(node)
    return node unless node.key?(:tag)
    case node[:macro]
    when 'glyph' then return nil
    when 'image' then return { 'name' => 'image', 'target' => node[:target] }
    when 'icon' then return { 'name' => 'icon', 'target' => node[:target] || plain(node).delete_prefix('[').delete_suffix(']') }
    when String then return { 'name' => node[:macro] }
    end
    inlines = node[:children].map { |c| to_asg(c) }.compact
    case node[:tag]
    when 'a'
      href = CGI.unescapeHTML(attr(node[:attrs], 'href') || '')
      return { 'name' => 'anchor', 'id' => attr(node[:attrs], 'id') } if href.empty? && inlines.empty?
      variant = href.start_with?('#') ? 'xref' : 'link'
      with_id(node, { 'name' => 'ref', 'variant' => variant, 'target' => href.delete_prefix('#'), 'inlines' => inlines })
    when 'span'
      # A role-only span carries no semantics of its own.
      with_id(node, { 'name' => 'span', 'variant' => 'generic', 'inlines' => inlines })
    else
      with_id(node, { 'name' => 'span', 'variant' => SPANS.fetch(node[:tag]), 'inlines' => inlines })
    end
  end

  def self.with_id(node, out)
    id = attr(node[:attrs], 'id')
    out['id'] = id if id
    out
  end
end

module Blocks
  LEAF = { paragraph: 'paragraph', listing: 'listing', literal: 'literal', pass: 'pass',
           stem: 'stem', verse: 'verse' }.freeze
  PARENT = { admonition: 'admonition', example: 'example', sidebar: 'sidebar', open: 'open',
             quote: 'quote' }.freeze

  def self.title(node)
    return nil unless node.title?
    Inlines.parse(node.title)
  end

  def self.common(node, out)
    out['id'] = node.id if node.id
    t = title(node)
    out['title'] = t if t
    out
  end

  # The ASG has no preamble node: its blocks are the document's own.
  def self.children(node)
    node.blocks.flat_map { |b| b.context == :preamble ? children(b) : [block(b)] }
  end

  # Body attribute entries take effect as each block converts
  # (`AbstractBlock#convert` plays them back), in document order.
  def self.block(node)
    node.document.playback_attributes node.attributes
    case node.context
    when :section
      common(node, { 'name' => 'section', 'level' => node.level, 'title' => Inlines.parse(node.title),
                     'blocks' => children(node) })
    when :floating_title
      common(node, { 'name' => 'heading', 'level' => node.level })
    when *LEAF.keys
      # Pass and stem content is the source; a listing or literal is read from
      # its converted content, where callouts are `<b class="conum">`.
      inlines = if %i[pass stem].include?(node.context)
                  [{ 'name' => 'text', 'value' => node.source }]
                else
                  Inlines.parse(node.content.to_s)
                end
      name = node.style == 'source' ? 'listing' : LEAF.fetch(node.context)
      common(node, { 'name' => name, 'inlines' => inlines })
    when *PARENT.keys
      out = { 'name' => PARENT.fetch(node.context), 'blocks' => children(node) }
      out['variant'] = node.attr('name') if node.context == :admonition
      if node.blocks.empty? && node.content_model == :simple
        out['blocks'] = [{ 'name' => 'paragraph', 'inlines' => Inlines.parse(node.content.to_s) }]
      end
      common(node, out)
    when :ulist, :olist, :colist
      variant = { ulist: 'unordered', olist: 'ordered', colist: 'callout' }.fetch(node.context)
      common(node, { 'name' => 'list', 'variant' => variant, 'items' => node.items.map { |i| list_item(i) } })
    when :dlist
      items = node.items.map do |terms, dd|
        item = { 'name' => 'dlistItem', 'terms' => terms.map { |t| Inlines.parse(t.text) } }
        if dd
          item['principal'] = Inlines.parse(dd.text) if dd.text?
          item['blocks'] = children(dd)
        end
        item
      end
      common(node, { 'name' => 'dlist', 'items' => items })
    when :thematic_break then common(node, { 'name' => 'break', 'variant' => 'thematic' })
    when :page_break then common(node, { 'name' => 'break', 'variant' => 'page' })
    when :image, :audio, :video
      common(node, { 'name' => node.context.to_s, 'target' => node.attr('target') })
    when :toc then common(node, { 'name' => 'toc' })
    when :table then common(node, table(node))
    else
      { 'name' => "unmapped:#{node.context}" }
    end
  end

  def self.list_item(item)
    out = { 'name' => 'listItem', 'principal' => Inlines.parse(item.text.to_s),
            'blocks' => children(item) }
    out['checked'] = item.attr?('checked') if item.attr?('checkbox')
    out
  end

  def self.table(node)
    rows = lambda do |section|
      node.rows.send(section).map do |row|
        row.map do |cell|
          # `Table::Cell#content`: no paragraph for an empty cell, one per
          # blank-line-separated part; the cell style is the cell's, not a span.
          content = if cell.style == :asciidoc
                      children(cell.inner_document)
                    else
                      text = cell.text
                      paras = text.include?("\n\n") ? text.split(Asciidoctor::BlankLineRx) : (text.empty? ? [] : [text])
                      paras.map { |para| { 'name' => 'paragraph', 'inlines' => Inlines.parse(para) } }
                    end
          { 'colspan' => cell.colspan || 1, 'rowspan' => cell.rowspan || 1, 'blocks' => content }
        end
      end
    end
    { 'name' => 'table', 'header' => rows.call(:head), 'rows' => rows.call(:body), 'footer' => rows.call(:foot) }
  end

  # The manpage parser turns the Name section into `mannames` / `manpurpose`;
  # the converter draws it back as the first section (`generate_manname_section`).
  def self.manname_section(doc)
    title = doc.attr 'manname-title', 'Name'
    first = doc.sections[0]
    title = title.upcase if first && first.title == first.title.upcase
    out = { 'name' => 'section', 'level' => 1, 'title' => Inlines.parse(title),
            'blocks' => [{ 'name' => 'paragraph',
                           'inlines' => Inlines.parse("#{(doc.attr 'mannames').join ', '} - #{doc.attr 'manpurpose'}") }] }
    out['id'] = doc.attr 'manname-id' if doc.attr? 'manname-id'
    out
  end

  def self.document(doc)
    blocks = children(doc)
    blocks.unshift manname_section(doc) if doc.doctype == 'manpage' && doc.attr?('manpurpose')
    out = { 'name' => 'document', 'blocks' => blocks }
    out['title'] = Inlines.parse(doc.doctitle) if doc.header? && doc.doctitle
    out
  end
end

$stdout.sync = true
$stdin.each_line do |line|
  src = JSON.parse(line)['src']
  logger = CapturingLogger.new
  Asciidoctor::LoggerManager.logger = logger
  reply = begin
    doc = Asciidoctor.load(src, safe: :unsafe, sourcemap: true)
    JSON.generate({ 'asg' => Blocks.document(doc), 'warnings' => logger.messages })
  rescue StandardError => e
    JSON.generate({ 'error' => "#{e.class}: #{e.message}".scrub })
  end
  puts reply
end
