//! Property-based tests for the acdc parser
//!
//! These tests verify invariants that should hold for ANY input, not just
//! specific fixtures. They complement the fixture-based tests by finding
//! edge cases and ensuring the parser is robust against unexpected inputs.

#[cfg(feature = "asg")]
mod asciidoctor_diff;
mod documents;
mod generators;
mod invariants;
mod source_locations;
