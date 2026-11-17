mod peg;

use peg::conditional_parser;

use std::path::Path;

use std::collections::HashMap;

use crate::{
    CompatibilityMode, DocumentAttributeValue, DocumentAttributes, Location, Substitution,
    error::{Error, SourceLocation},
    model::{HEADER, Position, apply_attribute_substitutions},
};

#[derive(Debug)]
pub(super) struct Conditional<'input> {
    condition: Condition<'input>,
    content: Option<&'input str>,
}

#[derive(Debug)]
enum Condition<'input> {
    Ifdef(AttributeCondition<'input>),
    Ifndef(AttributeCondition<'input>),
    Ifeval(EvalCondition),
    AsciidoctorIfeval(AsciidoctorEval<'input>),
    Unevaluable(Option<AttributeCondition<'input>>),
}

/// An `ifeval` expression read as asciidoctor reads it (`reader.rb`
/// `preprocess_conditional_directive`): `EvalExpressionRx`,
/// `/^(.+?) *([=!><]=|[><]) *(.+)$/` over the stripped bracket text.
#[derive(Debug)]
struct AsciidoctorEval<'input> {
    lhs: &'input str,
    operator: Operator,
    rhs: &'input str,
}

/// An operand as asciidoctor's `resolve_expr_val` returns it.
#[derive(Debug, PartialEq)]
enum RubyValue {
    String(String),
    Nil,
    Boolean(bool),
    Integer(i64),
    Float(f64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Operation {
    Or,
    And,
}

#[derive(Debug, PartialEq)]
struct AttributeCondition<'input> {
    attributes: Vec<&'input str>,
    operation: Option<Operation>,
}

#[derive(Debug)]
struct EvalCondition {
    left: EvalValue,
    operator: Operator,
    right: EvalValue,
}

#[derive(Debug, Clone, PartialEq, PartialOrd)]
enum EvalValue {
    String(String),
    Number(f64),
    Boolean(bool),
}

#[derive(Debug, PartialEq)]
enum Operator {
    Equal,
    NotEqual,
    LessThan,
    GreaterThan,
    LessThanOrEqual,
    GreaterThanOrEqual,
}

#[derive(Debug)]
pub(super) struct Endif<'input> {
    condition: Option<AttributeCondition<'input>>,
}

impl<'input> AttributeCondition<'input> {
    fn new(attributes: Vec<&'input str>, operation: Option<Operation>) -> Self {
        Self {
            attributes,
            operation,
        }
    }

    fn matches(&self, other: &Self) -> bool {
        self.operation == other.operation
            && self.attributes.len() == other.attributes.len()
            && self
                .attributes
                .iter()
                .zip(&other.attributes)
                .all(|(left, right)| left.eq_ignore_ascii_case(right))
    }
}

impl<'input> Conditional<'input> {
    /// An invalid opening still owns its block. Keep a recognizable target so
    /// a named terminator can close it, but never expose an unevaluated body.
    pub(super) fn unevaluable(line: &'input str) -> Self {
        let body = line.split_once("::").map_or("", |(_, body)| body);
        let (target, content) = body.split_once('[').unwrap_or((body, ""));
        let inline = !line.starts_with("ifeval::")
            && content
                .strip_suffix(']')
                .is_some_and(|text| !text.is_empty());
        Self {
            condition: Condition::Unevaluable(conditional_parser::attribute_condition(target).ok()),
            content: inline.then_some(""),
        }
    }

    fn evaluate_attributes(
        attrs: &[&str],
        operation: Option<&Operation>,
        doc_attrs: &DocumentAttributes,
        negate: bool,
    ) -> bool {
        if attrs.is_empty() {
            tracing::warn!("no attributes in conditional directive but expecting at least one");
            return !negate; // ifdef: false, ifndef: true
        }

        let result = match operation {
            Some(Operation::Or) => attrs.iter().any(|attr| doc_attrs.contains_key(attr)),
            _ => attrs.iter().all(|attr| doc_attrs.contains_key(attr)),
        };

        if negate { !result } else { result }
    }

    pub(super) fn is_true(
        &self,
        attributes: &DocumentAttributes,
        content: &mut String,
        line_number: usize,
        current_offset: usize,
        file_parent: Option<&Path>,
    ) -> Result<bool, Error> {
        let is_true = match &self.condition {
            Condition::Ifdef(condition) => Self::evaluate_attributes(
                &condition.attributes,
                condition.operation.as_ref(),
                attributes,
                false,
            ),
            Condition::Ifndef(condition) => Self::evaluate_attributes(
                &condition.attributes,
                condition.operation.as_ref(),
                attributes,
                true,
            ),
            Condition::Ifeval(ifeval) => {
                ifeval.evaluate(attributes, line_number, current_offset, file_parent)?
            }
            Condition::AsciidoctorIfeval(ifeval) => ifeval.evaluate(attributes),
            Condition::Unevaluable(_) => false,
        };
        if is_true && let Some(if_content) = &self.content {
            content.push_str(if_content);
        }
        Ok(is_true)
    }

    pub(super) fn has_inline_content(&self) -> bool {
        self.content.is_some()
    }

    fn attribute_condition(&self) -> Option<&AttributeCondition<'_>> {
        match &self.condition {
            Condition::Ifdef(condition) | Condition::Ifndef(condition) => Some(condition),
            Condition::Ifeval(_) | Condition::AsciidoctorIfeval(_) => None,
            Condition::Unevaluable(condition) => condition.as_ref(),
        }
    }
}

impl Endif<'_> {
    #[tracing::instrument(level = "trace", skip_all)]
    pub(super) fn closes(&self, conditional: &Conditional<'_>) -> bool {
        match (&self.condition, conditional.attribute_condition()) {
            (None, _) => true,
            (Some(endif), Some(opening)) => endif.matches(opening),
            (Some(_), None) => false,
        }
    }
}

impl EvalCondition {
    #[tracing::instrument(level = "trace", skip_all, fields(line_number = line_number, current_offset = current_offset))]
    fn evaluate(
        &self,
        attributes: &DocumentAttributes,
        line_number: usize,
        current_offset: usize,
        file_parent: Option<&Path>,
    ) -> Result<bool, Error> {
        let left = self.left.convert(attributes);
        let right = self.right.convert(attributes);

        // TOOD(nlopes): There are a few better ways to do this, but for now, this is
        // fine. I'm just going for functionality.
        match (&left, &right) {
            (EvalValue::Number(_), EvalValue::Number(_))
            | (EvalValue::Boolean(_), EvalValue::Boolean(_))
            | (EvalValue::String(_), EvalValue::String(_)) => {}
            _ => {
                return Err(Error::InvalidIfEvalDirectiveMismatchedTypes(Box::new(
                    SourceLocation {
                        file: file_parent.map(Path::to_path_buf),
                        location: Location::point(Position::from_line_col(line_number, 1)),
                    },
                )));
            }
        }

        Ok(match self.operator {
            Operator::Equal => left == right,
            Operator::NotEqual => left != right,
            Operator::LessThan => left < right,
            Operator::GreaterThan => left > right,
            Operator::LessThanOrEqual => left <= right,
            Operator::GreaterThanOrEqual => left >= right,
        })
    }
}

impl EvalValue {
    #[tracing::instrument(level = "trace", skip_all)]
    fn convert(&self, attributes: &DocumentAttributes) -> Self {
        match self {
            EvalValue::String(s) => {
                // First we substitute any attributes in the string with their values
                let s = apply_attribute_substitutions(s, HEADER, attributes);

                // Quotes select string comparisons even for numbers and booleans.
                if let Some(value) = Self::strip_quotes(&s) {
                    return EvalValue::String(value.to_string());
                }

                // Try to parse as bool, f64, or evaluate as expression, otherwise return as string
                s.parse::<bool>()
                    .map(EvalValue::Boolean)
                    .or_else(|_| s.parse::<f64>().map(EvalValue::Number))
                    .or_else(|_| evalexpr::eval_float(&s).map(EvalValue::Number))
                    .or_else(|_| {
                        #[allow(clippy::cast_precision_loss)]
                        evalexpr::eval_int(&s)
                            .map(|v| v as f64)
                            .map(EvalValue::Number)
                    })
                    .unwrap_or_else(|_| EvalValue::String(s.into_owned()))
            }
            value @ (EvalValue::Number(_) | EvalValue::Boolean(_)) => value.clone(),
        }
    }

    #[tracing::instrument(level = "trace", skip_all)]
    fn strip_quotes(s: &str) -> Option<&str> {
        s.strip_prefix('\'')
            .and_then(|value| value.strip_suffix('\''))
            .or_else(|| s.strip_prefix('"')?.strip_suffix('"'))
    }
}

impl<'input> AsciidoctorEval<'input> {
    /// `ifeval::[text]` whose stripped text matches `EvalExpressionRx`: the shortest left
    /// operand followed by spaces and an operator (`[=!><]=` tried before `[><]`), then
    /// spaces and a right operand of at least one character.
    fn parse(line: &'input str) -> Option<Self> {
        const OPERATORS: [(&str, Operator); 6] = [
            ("==", Operator::Equal),
            ("!=", Operator::NotEqual),
            ("<=", Operator::LessThanOrEqual),
            (">=", Operator::GreaterThanOrEqual),
            ("<", Operator::LessThan),
            (">", Operator::GreaterThan),
        ];
        let text = line.strip_prefix("ifeval::[")?.strip_suffix(']')?;
        let text = text.trim_matches(is_ruby_space);
        for (index, _) in text.char_indices().skip(1) {
            let rest = text[index..].trim_start_matches(' ');
            for (token, operator) in OPERATORS {
                let Some(after) = rest.strip_prefix(token) else {
                    continue;
                };
                // ` *(.+)$`: the spaces give one back when nothing else follows.
                let rhs = match after.trim_start_matches(' ') {
                    "" => &after[after.len().saturating_sub(1)..],
                    rhs => rhs,
                };
                if !rhs.is_empty() {
                    return Some(Self { lhs: &text[..index], operator, rhs });
                }
            }
        }
        None
    }

    /// `lhs.send(op, rhs)`; an exception (an ordering between types Ruby cannot
    /// compare) skips the body, as asciidoctor's `rescue true`.
    fn evaluate(&self, attributes: &DocumentAttributes) -> bool {
        let left = RubyValue::resolve(self.lhs, attributes);
        let right = RubyValue::resolve(self.rhs, attributes);
        match self.operator {
            Operator::Equal => left.ruby_eq(&right),
            Operator::NotEqual => !left.ruby_eq(&right),
            Operator::LessThan => left.ruby_cmp(&right).is_some_and(std::cmp::Ordering::is_lt),
            Operator::GreaterThan => left.ruby_cmp(&right).is_some_and(std::cmp::Ordering::is_gt),
            Operator::LessThanOrEqual => left.ruby_cmp(&right).is_some_and(std::cmp::Ordering::is_le),
            Operator::GreaterThanOrEqual => {
                left.ruby_cmp(&right).is_some_and(std::cmp::Ordering::is_ge)
            }
        }
    }
}

/// Ruby's `String#strip` / `#rstrip` set: ASCII whitespace and NUL.
fn is_ruby_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r' | '\0')
}

impl RubyValue {
    /// `resolve_expr_val`: a quoted operand drops its first character only
    /// (`val.slice 1, (val.length - 1)`), references read the stored value with missing
    /// ones dropped, and an unquoted operand coerces as Ruby does.
    fn resolve(value: &str, attributes: &DocumentAttributes) -> Self {
        let quoted = (value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\''));
        let value = if quoted { &value[1..] } else { value };
        let value = if value.contains('{') {
            substitute_stored_values(value, attributes)
        } else {
            value.to_owned()
        };
        if quoted {
            Self::String(value)
        } else if value.is_empty() {
            Self::Nil
        } else if value == "true" {
            Self::Boolean(true)
        } else if value == "false" {
            Self::Boolean(false)
        } else if value.trim_end_matches(is_ruby_space).is_empty() {
            Self::String(" ".to_owned())
        } else if value.contains('.') {
            Self::Float(ruby_to_f(&value))
        } else {
            Self::Integer(ruby_to_i(&value))
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn ruby_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Integer(a), Self::Float(b)) | (Self::Float(b), Self::Integer(a)) => {
                (*a as f64) == *b
            }
            _ => self == other,
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn ruby_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Self::String(a), Self::String(b)) => Some(a.as_bytes().cmp(b.as_bytes())),
            (Self::Integer(a), Self::Integer(b)) => Some(a.cmp(b)),
            (Self::Float(a), Self::Float(b)) => a.partial_cmp(b),
            (Self::Integer(a), Self::Float(b)) => (*a as f64).partial_cmp(b),
            (Self::Float(a), Self::Integer(b)) => a.partial_cmp(&(*b as f64)),
            _ => None,
        }
    }
}

/// `sub_attributes(text, attribute_missing: 'drop')` over the values as asciidoctor
/// stores them: an entry's value already carries its special characters escaped.
fn substitute_stored_values(text: &str, attributes: &DocumentAttributes) -> String {
    let mut stored: HashMap<String, DocumentAttributeValue<'static>> = HashMap::new();
    let _ = crate::substitute_attributes(text, |name| {
        stored
            .entry(name.to_owned())
            .or_insert_with(|| DocumentAttributeValue::from(attributes.get(name).map(stored_text).unwrap_or_default()));
        None
    });
    crate::substitute_attributes(text, |name| stored.get(name)).into_owned()
}

fn stored_text(value: &DocumentAttributeValue) -> String {
    if value.text().is_none() {
        let mut text = String::new();
        let _ = value.write_text(&mut text);
        return text;
    }
    value
        .parts()
        .map(|part| {
            part.substitutions()
                .filter(|substitution| *substitution == Substitution::SpecialChars)
                .fold(part.text().to_owned(), |text, _| {
                    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
                })
        })
        .collect()
}

/// The longest prefix of `pattern`-matching characters, after leading whitespace.
fn ruby_numeric_prefix(s: &str, float: bool) -> String {
    let s = s.trim_start_matches(is_ruby_space);
    let bytes = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    if matches!(bytes.first(), Some(b'+' | b'-')) {
        out.push(char::from(bytes[0]));
        i = 1;
    }
    // Digits, an underscore allowed only between two digits.
    let digits = |out: &mut String, i: &mut usize| {
        let start = *i;
        while *i < bytes.len() {
            if bytes[*i].is_ascii_digit() {
                out.push(char::from(bytes[*i]));
                *i += 1;
            } else if bytes[*i] == b'_'
                && *i > start
                && bytes.get(*i + 1).is_some_and(u8::is_ascii_digit)
            {
                *i += 1;
            } else {
                break;
            }
        }
        *i > start
    };
    let whole = digits(&mut out, &mut i);
    if float {
        if bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
            out.push('.');
            i += 1;
            digits(&mut out, &mut i);
        } else if !whole {
            return String::new();
        }
        if matches!(bytes.get(i), Some(b'e' | b'E')) {
            let mut exponent = String::from("e");
            let mut j = i + 1;
            if matches!(bytes.get(j), Some(b'+' | b'-')) {
                exponent.push(char::from(bytes[j]));
                j += 1;
            }
            if digits(&mut exponent, &mut j) {
                out.push_str(&exponent);
            }
        }
    }
    out
}

/// Ruby `String#to_i`: the leading integer, `0` when there is none.
fn ruby_to_i(s: &str) -> i64 {
    let digits = ruby_numeric_prefix(s, false);
    digits.parse::<i64>().unwrap_or_else(|_| {
        if digits.chars().any(|c| c.is_ascii_digit()) {
            if digits.starts_with('-') { i64::MIN } else { i64::MAX }
        } else {
            0
        }
    })
}

/// Ruby `String#to_f`: the leading float, `0.0` when there is none.
fn ruby_to_f(s: &str) -> f64 {
    ruby_numeric_prefix(s, true).parse::<f64>().unwrap_or(0.0)
}

/// An `ifeval` the selected mode reads as asciidoctor does, when it is one.
pub(super) fn asciidoctor_ifeval(line: &str, mode: CompatibilityMode) -> Option<Conditional<'_>> {
    (mode == CompatibilityMode::Asciidoctor)
        .then(|| AsciidoctorEval::parse(line))
        .flatten()
        .map(|eval| Conditional {
            condition: Condition::AsciidoctorIfeval(eval),
            content: None,
        })
}

#[tracing::instrument(level = "trace", skip_all, fields(line_number = line_number, current_offset = current_offset))]
pub(super) fn parse_line<'input>(
    line: &'input str,
    line_number: usize,
    current_offset: usize,
    file_parent: Option<&Path>,
) -> Result<Conditional<'input>, Error> {
    conditional_parser::conditional(line).map_err(|_| {
        Error::InvalidConditionalDirective(Box::new(SourceLocation {
            file: file_parent.map(Path::to_path_buf),
            location: Location::point(Position::from_line_col(line_number, 1)),
        }))
    })
}

#[tracing::instrument(level = "trace", skip_all, fields(line_number = line_number, current_offset = current_offset))]
pub(super) fn parse_endif<'input>(
    line: &'input str,
    line_number: usize,
    current_offset: usize,
    file_parent: Option<&Path>,
) -> Result<Endif<'input>, Error> {
    conditional_parser::endif(line).map_err(|_| {
        Error::InvalidConditionalDirective(Box::new(SourceLocation {
            file: file_parent.map(Path::to_path_buf),
            location: Location::point(Position::from_line_col(line_number, 1)),
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ifdef_single_attribute() -> Result<(), Error> {
        let line = "ifdef::attribute[]";
        let conditional = parse_line(line, 1, 0, None)?;
        assert!(
            matches!(conditional, Conditional { condition: Condition::Ifdef(condition), content: None } if condition.attributes == vec!["attribute"] && condition.operation.is_none())
        );
        Ok(())
    }

    #[test]
    fn test_ifdef_or_attributes() -> Result<(), Error> {
        let line = "ifdef::attr1,attr2[]";
        let conditional = parse_line(line, 1, 0, None)?;
        assert!(
            matches!(conditional, Conditional { condition: Condition::Ifdef(condition), content: None } if condition.attributes == vec!["attr1", "attr2"] && condition.operation == Some(Operation::Or))
        );
        Ok(())
    }

    #[test]
    fn test_ifdef_and_attributes() -> Result<(), Error> {
        let line = "ifdef::attr1+attr2[]";
        let conditional = parse_line(line, 1, 0, None)?;
        assert!(
            matches!(conditional, Conditional { condition: Condition::Ifdef(condition), content: None } if condition.attributes == vec!["attr1", "attr2"] && condition.operation == Some(Operation::And))
        );
        Ok(())
    }

    #[test]
    fn test_ifdef_three_or_attributes() -> Result<(), Error> {
        let conditional = parse_line("ifdef::attr1,attr2,attr3[]", 1, 0, None)?;
        assert!(
            matches!(conditional, Conditional { condition: Condition::Ifdef(condition), .. } if condition.attributes == vec!["attr1", "attr2", "attr3"] && condition.operation == Some(Operation::Or))
        );
        Ok(())
    }

    #[test]
    fn test_ifdef_three_and_attributes() -> Result<(), Error> {
        let conditional = parse_line("ifdef::attr1+attr2+attr3[]", 1, 0, None)?;
        assert!(
            matches!(conditional, Conditional { condition: Condition::Ifdef(condition), .. } if condition.attributes == vec!["attr1", "attr2", "attr3"] && condition.operation == Some(Operation::And))
        );
        Ok(())
    }

    #[test]
    fn test_ifdef_rejects_mixed_attribute_operators() {
        assert!(matches!(
            parse_line("ifdef::attr1,attr2+attr3[]", 1, 0, None),
            Err(Error::InvalidConditionalDirective(..))
        ));
    }

    #[test]
    fn test_ifndef() -> Result<(), Error> {
        let line = "ifndef::attribute[]";
        let conditional = parse_line(line, 1, 0, None)?;
        assert!(
            matches!(conditional, Conditional { condition: Condition::Ifndef(condition), content: None } if condition.attributes == vec!["attribute"] && condition.operation.is_none())
        );
        Ok(())
    }

    #[test]
    fn effective_defaults_are_defined_for_conditionals() -> Result<(), Error> {
        let attributes = DocumentAttributes::default();
        let defined = parse_line("ifdef::max-include-depth[]", 1, 0, None)?;
        let undefined = parse_line("ifndef::max-include-depth[]", 1, 0, None)?;

        assert!(defined.is_true(&attributes, &mut String::new(), 1, 0, None)?);
        assert!(!undefined.is_true(&attributes, &mut String::new(), 1, 0, None)?);
        Ok(())
    }

    #[test]
    fn ifeval_uses_typed_default_text_presentation() -> Result<(), Error> {
        let conditional = parse_line("ifeval::[{max-include-depth} == 64]", 1, 0, None)?;

        assert!(conditional.is_true(
            &DocumentAttributes::default(),
            &mut String::new(),
            1,
            0,
            None
        )?);

        let mut attributes = DocumentAttributes::default();
        assert!(
            attributes
                .set("max-include-depth".into(), "03".into())
                .is_ok()
        );
        let explicit_condition = parse_line("ifeval::[{max-include-depth} == 3]", 1, 0, None)?;
        assert!(explicit_condition.is_true(&attributes, &mut String::new(), 1, 0, None)?);
        Ok(())
    }

    #[test]
    fn test_ifeval_simple_math() -> Result<(), Error> {
        let line = "ifeval::[1 + 1 == 2]";
        let conditional = parse_line(line, 1, 0, None)?;
        assert!(
            matches!(&conditional, Conditional { condition: Condition::Ifeval(ifeval), content: None } if ifeval.left == EvalValue::String("1 + 1".to_string()) && ifeval.operator == Operator::Equal && ifeval.right == EvalValue::String("2".to_string()))
        );
        assert!(conditional.is_true(
            &DocumentAttributes::default(),
            &mut String::new(),
            1,
            0,
            None
        )?);
        Ok(())
    }

    #[test]
    fn test_ifeval_str_equality() -> Result<(), Error> {
        let line = "ifeval::['ASDF' == ASDF]";
        let conditional = parse_line(line, 1, 0, None)?;
        assert!(
            matches!(&conditional, Conditional { condition: Condition::Ifeval(ifeval), content: None } if ifeval.left == EvalValue::String("'ASDF'".to_string()) && ifeval.operator == Operator::Equal && ifeval.right == EvalValue::String("ASDF".to_string()))
        );
        assert!(conditional.is_true(
            &DocumentAttributes::default(),
            &mut String::new(),
            1,
            0,
            None
        )?);
        Ok(())
    }

    #[test]
    fn test_ifeval_greater_than_string_vs_number() -> Result<(), Error> {
        let line = "ifeval::['1+1' >= 2]";
        let conditional = parse_line(line, 1, 0, None)?;
        assert!(
            matches!(&conditional, Conditional { condition: Condition::Ifeval(ifeval), content: None } if ifeval.left == EvalValue::String("'1+1'".to_string()) && ifeval.operator == Operator::GreaterThanOrEqual && ifeval.right == EvalValue::String("2".to_string()))
        );

        assert!(matches!(
            conditional.is_true(
                &DocumentAttributes::default(),
                &mut String::new(),
                1,
                0,
                None
            ),
            Err(Error::InvalidIfEvalDirectiveMismatchedTypes(..))
        ));
        Ok(())
    }

    #[test]
    fn quoted_ifeval_rejects_invalid_directives_without_panicking() {
        for line in [
            "ifeval::[\"<tag>\" \"<tag>\"]",
            "ifeval::[\"<tag>\" ==]",
            "ifeval::[\"<tag>\" == \"<tag>\"] trailing",
            "ifeval::[\"<tag> == \"<tag>\"]",
            "ifeval::[' == ']",
        ] {
            assert!(
                matches!(
                    parse_line(line, 1, 0, None),
                    Err(Error::InvalidConditionalDirective(..))
                ),
                "unexpected result for {line:?}"
            );
        }
    }

    #[test]
    fn quoted_ifeval_retains_mismatched_type_errors() -> Result<(), Error> {
        for line in ["ifeval::[\"2\" > 1]", "ifeval::['true' == true]"] {
            let conditional = parse_line(line, 1, 0, None)?;
            assert!(matches!(
                conditional.is_true(
                    &DocumentAttributes::default(),
                    &mut String::new(),
                    1,
                    0,
                    None
                ),
                Err(Error::InvalidIfEvalDirectiveMismatchedTypes(..))
            ));
        }
        Ok(())
    }

    #[test]
    fn test_ifdef_with_content() -> Result<(), Error> {
        let line = "ifdef::attribute[Some content here]";
        let conditional = parse_line(line, 1, 0, None)?;
        assert!(
            matches!(conditional, Conditional { condition: Condition::Ifdef(condition), content: Some(content) } if condition.attributes == vec!["attribute"] && condition.operation.is_none() && content == "Some content here")
        );
        Ok(())
    }

    #[test]
    fn test_endif() -> Result<(), Error> {
        let line = "endif::attribute[]";
        let endif = parse_endif(line, 1, 0, None)?;
        assert!(matches!(
            endif.condition,
            Some(AttributeCondition {
                attributes,
                operation: None,
            }) if attributes == ["attribute"]
        ));
        Ok(())
    }

    #[test]
    fn test_endif_no_attribute() -> Result<(), Error> {
        let line = "endif::[]";
        let endif = parse_endif(line, 1, 0, None)?;
        assert_eq!(endif.condition, None);
        Ok(())
    }

    #[test]
    fn test_endif_matches_complete_condition_case_insensitively() -> Result<(), Error> {
        let conditional = parse_line("ifdef::Backend-PDF,Backend-DocBook5[]", 1, 0, None)?;
        let endif = parse_endif("endif::backend-pdf,backend-docbook5[]", 2, 0, None)?;
        assert!(endif.closes(&conditional));
        Ok(())
    }

    #[test]
    fn test_endif_rejects_partial_or_reordered_condition() -> Result<(), Error> {
        let conditional = parse_line("ifdef::backend-pdf,backend-docbook5[]", 1, 0, None)?;
        let partial = parse_endif("endif::backend-pdf[]", 2, 0, None)?;
        let reordered = parse_endif("endif::backend-docbook5,backend-pdf[]", 2, 0, None)?;
        let different_operation = parse_endif("endif::backend-pdf+backend-docbook5[]", 2, 0, None)?;
        assert!(!partial.closes(&conditional));
        assert!(!reordered.closes(&conditional));
        assert!(!different_operation.closes(&conditional));
        Ok(())
    }
}
