//! Tables for the rules of `domain::comment`: what is mirrored and which
//! comments are service notices.

use bridge::domain::{
    AuthorType, Comment, RunOutcome, is_service_notice, is_system_notice, may_be_service_notice,
    should_forward,
};

fn comment(author: AuthorType, agent: Option<&str>, body: &str) -> Comment {
    Comment {
        id: "c".into(),
        body: body.into(),
        author_type: author,
        author_agent_id: agent.map(str::to_string),
        origin: Some("comment".into()),
        created_by_run_id: Some("run-1".into()),
        created_at: "2026-11-19T10:00:00.000Z".into(),
        deleted: false,
    }
}

fn run(status: &str, error: Option<&str>) -> RunOutcome {
    RunOutcome {
        status: status.into(),
        error: error.map(str::to_string),
    }
}

#[test]
fn should_forward_table() {
    let agent = |id: &str| comment(AuthorType::Agent, Some(id), "x");
    assert!(should_forward(&agent("a1"), "a1"));
    assert!(!should_forward(&agent("a2"), "a1"));
    assert!(!should_forward(
        &comment(AuthorType::Agent, None, "x"),
        "a1"
    ));
    assert!(should_forward(
        &comment(AuthorType::System, None, "x"),
        "a1"
    ));
    assert!(!should_forward(&comment(AuthorType::User, None, "x"), "a1"));
    assert!(!should_forward(
        &comment(AuthorType::Other, Some("a1"), "x"),
        "a1"
    ));
    let mut deleted = agent("a1");
    deleted.deleted = true;
    assert!(!should_forward(&deleted, "a1"));
    let mut deleted_system = comment(AuthorType::System, None, "x");
    deleted_system.deleted = true;
    assert!(!should_forward(&deleted_system, "a1"));
}

#[test]
fn author_types_are_parsed_strictly() {
    assert_eq!(AuthorType::parse("agent"), AuthorType::Agent);
    assert_eq!(AuthorType::parse("user"), AuthorType::User);
    assert_eq!(AuthorType::parse("system"), AuthorType::System);
    assert_eq!(AuthorType::parse("Agent"), AuthorType::Other);
    assert_eq!(AuthorType::parse(""), AuthorType::Other);
}

#[test]
fn is_service_notice_table() {
    let c = |body: &str| comment(AuthorType::Agent, Some("a1"), body);
    // (comment, run, expected)
    let cases = [
        (c("cancelled"), run("cancelled", None), true),
        (c("Cancelled"), run("CANCELLED", None), true),
        (c("failed"), run("failed", None), true),
        (c("timed_out"), run("timed_out", Some("other")), true),
        (c("Limit"), run("failed", Some("limit")), true),
        (c(" Limit "), run("failed", Some(" LIMIT \nmore")), true),
        // Not the first line of the error.
        (c("more"), run("failed", Some("limit\nmore")), false),
        // Not a failed run.
        (c("limit"), run("succeeded", Some("limit")), false),
        (c("running"), run("running", None), false),
        (c("queued"), run("queued", Some("queued")), false),
        // Failed run, ordinary text.
        (c("Готово"), run("failed", Some("limit")), false),
        (c("Готово"), run("cancelled", None), false),
        (
            c("limit reached today"),
            run("failed", Some("limit reached")),
            false,
        ),
    ];
    for (comment, run, expected) in cases {
        assert_eq!(
            is_service_notice(&comment, &run),
            expected,
            "{:?} vs {run:?}",
            comment.body
        );
    }
}

#[test]
fn system_comment_is_a_notice_by_itself() {
    assert!(is_system_notice(&comment(AuthorType::System, None, "x")));
    assert!(!is_system_notice(&comment(
        AuthorType::Agent,
        Some("a1"),
        "x"
    )));
    assert!(!is_system_notice(&comment(AuthorType::User, None, "x")));
}

#[test]
fn pre_check_before_the_run_lookup() {
    let agent = |body: &str| comment(AuthorType::Agent, Some("a1"), body);
    assert!(may_be_service_notice(&agent("one short line")));
    assert!(may_be_service_notice(&agent("  padded  ")));
    assert!(may_be_service_notice(&agent(&"x".repeat(300))));
    assert!(!may_be_service_notice(&agent(&"x".repeat(301))));
    assert!(!may_be_service_notice(&agent("two\nlines")));
    assert!(!may_be_service_notice(&agent("   ")));
    assert!(!may_be_service_notice(&comment(
        AuthorType::System,
        None,
        "x"
    )));
    assert!(!may_be_service_notice(&comment(
        AuthorType::User,
        None,
        "x"
    )));
    let mut no_run = agent("x");
    no_run.created_by_run_id = None;
    assert!(!may_be_service_notice(&no_run));
    let mut receipt = agent("x");
    receipt.origin = Some("fast_response".into());
    assert!(!may_be_service_notice(&receipt));
    // Characters, not bytes: 300 Cyrillic letters are 600 bytes.
    assert!(may_be_service_notice(&agent(&"я".repeat(300))));
}
