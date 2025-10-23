#![cfg(test)]

use crate::agent::events::ReasoningAggregator;

#[test]
fn take_text_none_when_empty() {
    let mut r = ReasoningAggregator::new();
    assert_eq!(r.take_text(), None, "empty aggregator should return None");
}

#[test]
fn append_and_take_single_section() {
    let mut r = ReasoningAggregator::new();
    r.append_delta("hello");
    r.append_delta(" world  "); // trailing spaces should be trimmed on take
    let out = r.take_text();
    assert_eq!(out.as_deref(), Some("hello world"));
    // subsequent take should be None (buffer cleared)
    assert_eq!(r.take_text(), None);
}

#[test]
fn multiple_sections_joined_with_blank_line() {
    let mut r = ReasoningAggregator::new();

    // first section
    r.append_delta("first line");
    r.append_delta("\nmore");
    r.section_break();

    // second section
    r.append_delta("second");
    r.append_delta(" section  ");
    // no explicit section_break here; current section should be included

    let out = r.take_text().unwrap();
    // sections should be separated by a blank line
    assert_eq!(out, "first line\nmore\n\nsecond section");
}

#[test]
fn empty_sections_are_skipped_on_aggregation() {
    let mut r = ReasoningAggregator::new();

    // whitespace-only current section then break; should be skipped
    r.append_delta("   \n  ");
    r.section_break();

    // real content
    r.append_delta("actual");
    // also test trailing whitespace trimming
    r.append_delta(" content  ");
    let out = r.take_text().unwrap();
    assert_eq!(out, "actual content");
}

#[test]
fn choose_final_text_prefers_longer_final() {
    let mut r = ReasoningAggregator::new();
    // aggregated shorter
    r.append_delta("short");
    r.section_break();

    // final text is longer -> should be chosen
    let chosen = r.choose_final_text(Some("this is longer".to_string()));
    assert_eq!(chosen.as_deref(), Some("this is longer"));
}

#[test]
fn choose_final_text_prefers_aggregated_when_longer() {
    let mut r = ReasoningAggregator::new();

    // build a longer aggregated text across sections
    r.append_delta("alpha");
    r.section_break();
    r.append_delta("beta gamma");
    // no final section_break needed; take_text/choose_final_text will include current

    let chosen = r.choose_final_text(Some("short".to_string()));
    assert_eq!(chosen.as_deref(), Some("alpha\n\nbeta gamma"));
}

#[test]
fn choose_final_text_handles_only_final() {
    let mut r = ReasoningAggregator::new();
    let chosen = r.choose_final_text(Some("only final".to_string()));
    assert_eq!(chosen.as_deref(), Some("only final"));
}

#[test]
fn choose_final_text_handles_both_none() {
    let mut r = ReasoningAggregator::new();
    // ensure aggregator has no content
    r.section_break();
    let chosen = r.choose_final_text(None);
    assert_eq!(chosen, None);
}

#[test]
fn take_text_trims_trailing_whitespace_and_preserves_internal_newlines() {
    let mut r = ReasoningAggregator::new();
    r.append_delta("line1  ");
    r.append_delta("\nline2\t\n");
    r.section_break();
    r.append_delta("  line3");
    r.append_delta("\n\nline4   ");
    let out = r.take_text().unwrap();
    // trailing spaces on each section trimmed, but internal newlines preserved
    assert_eq!(out, "line1\nline2\n\n  line3\n\nline4");
}
