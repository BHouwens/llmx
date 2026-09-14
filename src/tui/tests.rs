use crate::config::loader::parse;
use crate::config::ResourceHints;
use crate::system::assess;
use crate::system::MemoryVerdict;
use crate::tui::state::format_tokens;
use crate::tui::state::PickerState;

fn hints(estimated: Option<f64>, min: Option<f64>, recommended: Option<f64>) -> ResourceHints {
    ResourceHints {
        estimated_ram_gb: estimated,
        min_ram_gb: min,
        recommended_ram_gb: recommended,
    }
}

#[test]
fn memory_advice_is_unknown_without_hints_or_a_reading() {
    assert_eq!(assess(None, Some(64.0)), MemoryVerdict::Unknown);
    assert_eq!(
        assess(Some(&hints(Some(30.0), None, None)), None),
        MemoryVerdict::Unknown
    );
}

#[test]
fn a_model_below_the_stated_minimum_is_flagged() {
    let verdict = assess(Some(&hints(None, Some(48.0), None)), Some(32.0));
    let MemoryVerdict::Insufficient(detail) = verdict else {
        panic!("expected Insufficient, got {verdict:?}");
    };
    assert!(detail.contains("48"), "should quote the minimum: {detail}");
    assert!(detail.contains("32"), "should quote the machine: {detail}");
}

#[test]
fn a_model_that_nearly_fills_memory_is_called_tight() {
    // 39 GB estimated on a 48 GB machine leaves 9 GB — just over the line.
    assert_eq!(
        assess(Some(&hints(Some(39.0), Some(32.0), None)), Some(48.0)),
        MemoryVerdict::Comfortable
    );
    // 39 GB on 44 GB leaves 5 GB, which is not enough for the OS and editor.
    assert!(matches!(
        assess(Some(&hints(Some(39.0), Some(32.0), None)), Some(44.0)),
        MemoryVerdict::Tight(_)
    ));
}

#[test]
fn an_estimate_larger_than_the_machine_is_insufficient() {
    assert!(matches!(
        assess(Some(&hints(Some(300.0), None, None)), Some(52.0)),
        MemoryVerdict::Insufficient(_)
    ));
}

#[test]
fn a_comfortable_profile_says_so() {
    assert_eq!(
        assess(Some(&hints(Some(22.0), Some(16.0), Some(32.0))), Some(64.0)),
        MemoryVerdict::Comfortable
    );
}

#[test]
fn token_counts_render_compactly() {
    assert_eq!(format_tokens(None), "—");
    assert_eq!(format_tokens(Some(131_072)), "128k");
    assert_eq!(format_tokens(Some(262_144)), "256k");
    assert_eq!(format_tokens(Some(512)), "512");
}

#[test]
fn selection_wraps_in_both_directions() {
    let config = parse(include_str!("../../examples/models.toml")).expect("examples parse");
    let count = config.models.len();
    let mut state = PickerState::new(
        config.models,
        std::path::PathBuf::from("/tmp/project"),
        std::path::PathBuf::from("/tmp/models.toml"),
        Some(64.0),
    );

    assert_eq!(state.selected, 0);
    state.move_up();
    assert_eq!(
        state.selected,
        count - 1,
        "up from the top wraps to the end"
    );
    state.move_down();
    assert_eq!(state.selected, 0, "down from the end wraps to the top");
}

#[test]
fn navigating_an_empty_list_does_not_panic() {
    let mut state = PickerState::new(
        Vec::new(),
        std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from("/tmp/models.toml"),
        None,
    );
    state.move_up();
    state.move_down();
    assert!(state.selected_row().is_none());
}
