//! The final launch boundary: what an authorization is bound to, and what
//! approvals show and record.

use super::*;
use crate::runtime::displayable;

fn ticket(response: BrokerResponse) -> askkey_protocol::ApprovalTicket {
    let BrokerResponse::Ok(BrokerPayload::Run(RunResult::ApprovalRequired { mut tickets, .. })) =
        response
    else {
        panic!("expected approvalRequired, got {response:?}");
    };
    tickets.remove(0)
}

#[test]
fn format_and_separator_characters_are_escaped() {
    for deceptive in [
        '\u{202e}',
        '\u{2066}',
        '\u{200b}',
        '\u{feff}',
        '\u{2028}',
        '\u{2029}',
        '\u{ad}',
        '\u{e0041}',
        '\n',
    ] {
        let shown = displayable(&format!("a{deceptive}b"));
        assert!(
            !shown.contains(deceptive),
            "{deceptive:?} shown as {shown:?}"
        );
        assert!(shown.starts_with("a\\"), "{shown:?}");
    }
    assert_eq!(
        displayable("git.exe push café 中文"),
        "git.exe push café 中文"
    );
}

#[tokio::test]
async fn every_shown_field_is_display_safe() {
    let (broker, provider, _) = setup();
    provider.add("Code\u{202e}", Access::Ask, "SYNTH_CODE", "1");
    let mut request = exit_with("SYNTH_CODE", "op-display", &["Code\u{202e}"]);
    request.caller_name = Some("Agent\u{2028}Ask Key".into());
    request.caller_purpose = Some("\u{2066}purpose".into());
    let _ = ticket(run(&broker, request).await);
    let summary = &broker.pending_approvals()[0];
    let shown = [
        summary.command_line.as_str(),
        summary.executable.as_str(),
        summary.working_directory.as_str(),
        summary.credential_names[0].as_str(),
        summary.caller_name.as_deref().unwrap(),
        summary.caller_purpose.as_deref().unwrap(),
    ];
    for field in shown {
        assert!(
            !field
                .chars()
                .any(|c| matches!(c, '\u{202e}' | '\u{2028}' | '\u{2066}')),
            "{field:?}"
        );
    }
}

#[tokio::test]
async fn approval_is_bound_to_the_resolved_executable() {
    let (broker, provider, _) = setup();
    provider.add("Code", Access::Ask, "SYNTH_CODE", "5");
    let earlier = tempfile::tempdir().unwrap();
    let mut request = exit_with("SYNTH_CODE", "op-path", &["Code"]);
    let root = system_root();
    request.inherited_environment.insert(
        "PATH".into(),
        format!(r"{};{root}\System32", earlier.path().display()),
    );
    let first = ticket(run(&broker, request.clone()).await);
    let shown = broker.pending_approvals()[0].executable.clone();
    assert!(
        shown.to_ascii_lowercase().ends_with(r"system32\cmd.exe"),
        "{shown}"
    );
    broker.decide(&first.request_id, true).unwrap();

    // A different cmd.exe now comes first on the same PATH.
    std::fs::copy(
        format!(r"{root}\System32\cmd.exe"),
        earlier.path().join("cmd.exe"),
    )
    .unwrap();
    assert_eq!(
        run(&broker, request.clone()).await,
        BrokerResponse::Error(ErrorCode::RequestRejected)
    );
    assert!(provider.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancelling_a_claimed_approval_prevents_launch() {
    let (broker, provider, _) = setup();
    provider.add("Code", Access::Ask, "SYNTH_CODE", "3");
    let request = exit_with("SYNTH_CODE", "op-claim", &["Code"]);
    let issued = ticket(run(&broker, request.clone()).await);
    broker.decide(&issued.request_id, true).unwrap();

    // The retry claims the approval, then a cancel arrives before the final
    // boundary consumes it.
    let Ok(approvals::Begin::Approved(_)) = broker.approvals.begin("op-claim", &request.digest())
    else {
        panic!("expected the approval to be claimed");
    };
    broker
        .approvals
        .cancel(&issued.request_id, &issued.capability)
        .unwrap();
    assert_eq!(
        broker.approvals.consume("op-claim"),
        Err(ErrorCode::RequestRejected)
    );
}

#[tokio::test]
async fn allow_run_is_bound_to_what_it_resolved() {
    let (broker, provider, _) = setup();
    provider.add("Code", Access::Allow, "SYNTH_CODE", "8");
    let request = exit_with("SYNTH_CODE", "op-allow-bound", &["Code"]);
    let directory = std::path::PathBuf::from(system_root());
    assert_eq!(
        broker.approvals.begin("op-allow-bound", &request.digest()),
        Ok(approvals::Begin::New)
    );
    // Resolved at revision 1, then edited before the final boundary.
    let stale = approvals::Binding {
        credentials: vec![("id-Code".into(), "1".into())],
        executable: format!(r"{}\System32\cmd.exe", system_root()).into(),
    };
    provider.edit("Code", "9");
    broker.approvals.start("op-allow-bound", stale.clone());
    let launched = broker
        .launch(
            &request,
            &directory,
            &stale,
            false,
            std::process::id(),
            std::future::pending(),
        )
        .await;
    assert_eq!(launched, Err(ErrorCode::RequestRejected));
    assert!(!provider.delivered.load(Ordering::SeqCst));

    // A fresh operation resolves the current revision and launches.
    broker.approvals.abandon("op-allow-bound");
    assert_eq!(run(&broker, request).await, exited(9));
}

#[tokio::test]
async fn records_name_the_resolved_credential() {
    let (broker, provider, _) = setup();
    provider.add("Code", Access::Allow, "SYNTH_CODE", "4");
    assert_eq!(
        run(&broker, exit_with("SYNTH_CODE", "op-record", &["code"])).await,
        exited(4)
    );
    assert_eq!(
        provider.events.lock().unwrap()[0].credential_names,
        ["Code"]
    );
}
