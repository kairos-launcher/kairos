//! Protocol tests for `ExternalPlugin`, driven by the scriptable fixture in `testdata/mock/`.

use super::*;

use crate::test_support::python3;

fn mock_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/plugin/testdata/mock")
}

/// A marker file path the mock appends lifecycle events to, in a folder that lives as long as
/// the returned guard.
fn marker() -> (tempfile::TempDir, PathBuf) {
    let tmp = crate::test_support::tmp();
    let p = tmp.path().join("mock.log");
    (tmp, p)
}

fn events(marker: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(marker).unwrap_or_default().lines().map(String::from).collect()
}

fn mock_with(edit: impl FnOnce(&mut Manifest), settings: Value) -> ExternalPlugin {
    let mut m = Manifest::load(&mock_dir()).unwrap();
    edit(&mut m);
    // Each run gets a fresh data folder under the system temp dir, never the user's state.
    let data = std::env::temp_dir().join(format!("kairos-test-data-{}", std::process::id())).join(&m.id);
    ExternalPlugin::with_data_dir(m, mock_dir(), settings, data)
}

fn mock() -> ExternalPlugin {
    mock_with(|_| {}, json!({}))
}

fn ask(p: &ExternalPlugin, text: &str) -> Vec<ResultItem> {
    p.query(&Query { text, trigger: Some("mock "), limit: 8 })
}

fn pid(p: &ExternalPlugin) -> String {
    let items = ask(p, "pid");
    assert_eq!(items.len(), 1, "pid query failed");
    items[0].title.clone()
}

#[test]
fn initialize_receives_settings() {
    if !python3() {
        return;
    }
    let p = mock_with(|_| {}, json!({"x": 1, "name": "n"}));
    let items = ask(&p, "settings");
    let got: Value = serde_json::from_str(&items[0].title).unwrap();
    assert_eq!(got, json!({"x": 1, "name": "n"}));
}

#[test]
fn initialize_gives_a_private_data_folder() {
    if !python3() {
        return;
    }
    let tmp = crate::test_support::tmp();
    let data = tmp.path().join("state/plugins/mock");
    let p = ExternalPlugin::with_data_dir(Manifest::load(&mock_dir()).unwrap(), mock_dir(), json!({}), data.clone());
    assert_eq!(ask(&p, "data_dir")[0].title, data.to_string_lossy());
    assert!(data.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&data).unwrap().permissions().mode() & 0o777, 0o700);
    }
}

#[test]
fn non_json_lines_and_foreign_messages_are_ignored() {
    if !python3() {
        return;
    }
    let p = mock();
    let first = pid(&p);
    let items = ask(&p, "noise");
    assert_eq!(items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["noise"]);
    assert_eq!(pid(&p), first, "noise is not a protocol failure");
}

#[test]
fn icon_paths_are_resolved_against_the_plugin_folder() {
    if !python3() {
        return;
    }
    let p = mock();
    let items = ask(&p, "icon");
    let path = |id: &str| items.iter().find(|i| i.id == id).unwrap().icon.as_ref().unwrap().path.clone();
    assert_eq!(path("rel").unwrap(), mock_dir().join("img/a.png").to_string_lossy());
    // On Windows a rooted path without a drive keeps the plugin folder's drive.
    #[cfg(windows)]
    let abs = format!("{}/abs/b.png", &mock_dir().to_string_lossy()[..2]);
    #[cfg(not(windows))]
    let abs = "/abs/b.png";
    assert_eq!(path("abs").unwrap(), abs);
    assert_eq!(items.iter().find(|i| i.id == "lazy").unwrap().icon.as_ref().unwrap().id.as_deref(), Some("known"));

    let lazy = p.icon("known").expect("known icon");
    assert_eq!(lazy.path.unwrap(), mock_dir().join("x.png").to_string_lossy());
    assert!(p.icon("unknown").is_none(), "null reply means no icon");
}

#[test]
fn streaming_merges_batches_and_drops_stale_qids() {
    if !python3() {
        return;
    }
    let p = mock();
    let items = ask(&p, "stream:3");
    let ids: Vec<_> = items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["r0", "r1", "r2", "r3"]);
}

#[test]
fn streaming_without_done_stops_at_the_budget() {
    if !python3() {
        return;
    }
    let p = mock_with(|m| m.query.timeout_ms = 300, json!({}));
    pid(&p); // start the process outside the measured query
    let t = Instant::now();
    let items = ask(&p, "stream-nodone");
    let took = t.elapsed();
    assert_eq!(items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["partial"]);
    assert!(took >= Duration::from_millis(250), "waited for more batches: {took:?}");
    assert!(took < Duration::from_millis(1000), "gave up near the budget: {took:?}");
}

#[test]
fn timeout_keeps_the_process_and_skips_the_late_reply() {
    if !python3() {
        return;
    }
    let p = mock_with(|m| m.query.timeout_ms = 200, json!({}));
    let first = pid(&p);
    let t = Instant::now();
    assert!(ask(&p, "sleep:500").is_empty(), "missed the budget");
    assert!(t.elapsed() < Duration::from_millis(450), "{:?}", t.elapsed());
    std::thread::sleep(Duration::from_millis(500)); // let the late reply arrive
    assert_eq!(pid(&p), first, "same process, and its answer is not the late `sleep` reply");
}

#[test]
fn error_reply_restarts_the_plugin() {
    if !python3() {
        return;
    }
    let p = mock();
    let first = pid(&p);
    assert!(ask(&p, "error").is_empty());
    assert_ne!(pid(&p), first);
}

#[test]
fn crash_restarts_the_plugin() {
    if !python3() {
        return;
    }
    let p = mock();
    let first = pid(&p);
    assert!(ask(&p, "crash").is_empty());
    assert_ne!(pid(&p), first);
}

#[test]
fn an_oversized_reply_restarts_the_plugin_instead_of_filling_memory() {
    if !python3() {
        return;
    }
    let p = mock();
    let first = pid(&p);
    assert!(ask(&p, "huge").is_empty());
    assert_ne!(pid(&p), first, "the plugin was stopped and starts afresh");
}

#[test]
fn execute_replies() {
    if !python3() {
        return;
    }
    let p = mock();
    assert!(!p.execute("r", "ok", "q").unwrap().keeps_open());
    assert!(p.execute("r", "stay", "q").unwrap().keeps_open());
    assert_eq!(p.execute("r", "fail", "q").err().map(|e| e.to_string()).as_deref(), Some("nope"));
    assert_eq!(p.execute("r", "fail-nomsg", "q").err().map(|e| e.to_string()).as_deref(), Some("action failed"));
}

#[test]
fn reap_idle_shuts_down_idle_on_demand_plugins() {
    if !python3() {
        return;
    }
    let (_marker, mk) = marker();
    let p = mock_with(|m| m.run.idle_timeout_s = 0, json!({"marker": mk}));
    let first = pid(&p);
    p.reap_idle();
    assert_eq!(events(&mk), [format!("init {first}"), format!("shutdown {first}")]);
    assert_ne!(pid(&p), first, "restarted on the next query");
}

#[test]
fn reap_idle_leaves_busy_and_resident_plugins_alone() {
    if !python3() {
        return;
    }
    let (_marker, mk) = marker();
    let busy = mock_with(|m| m.run.idle_timeout_s = 3600, json!({"marker": mk}));
    let first = pid(&busy);
    busy.reap_idle();
    assert_eq!(pid(&busy), first, "not idle long enough");

    let resident = mock_with(
        |m| {
            m.run.mode = Mode::Resident;
            m.run.idle_timeout_s = 0;
        },
        json!({"marker": mk}),
    );
    let first = pid(&resident);
    resident.reap_idle();
    assert_eq!(pid(&resident), first, "resident plugins are never reaped");
    assert!(!events(&mk).iter().any(|e| e.starts_with("shutdown")), "{:?}", events(&mk));
}

#[test]
fn warm_up_starts_only_resident_plugins() {
    if !python3() {
        return;
    }
    let (_marker, mk) = marker();
    mock_with(|_| {}, json!({"marker": mk})).warm_up();
    assert!(events(&mk).is_empty(), "on-demand waits for its first query");
    let p = mock_with(|m| m.run.mode = Mode::Resident, json!({"marker": mk}));
    p.warm_up();
    let ev = events(&mk);
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0], format!("init {}", pid(&p)), "the query reuses the warm process");
}

#[test]
fn spawn_failure_yields_no_results_and_an_execute_error() {
    let p = mock_with(
        |m| m.run.exec = Some(super::super::manifest::Exec::Argv(vec!["./does-not-exist".into()])),
        json!({}),
    );
    assert!(ask(&p, "pid").is_empty());
    let err = p.execute("r", "ok", "q").err().unwrap().to_string();
    assert!(err.contains("spawn mock"), "{err}");
}

#[cfg(target_os = "linux")]
#[test]
fn drop_kills_the_process() {
    if !python3() {
        return;
    }
    let p = mock();
    let id = pid(&p);
    let proc_dir = PathBuf::from("/proc").join(&id);
    assert!(proc_dir.exists());
    drop(p);
    assert!(!proc_dir.exists(), "child {id} still running after drop");
}

// Pure helpers.

#[test]
fn collect_notes_filters_by_method_and_qid() {
    let note = |method: &str, qid: u64, done: Option<bool>| {
        let mut params = json!({"qid": qid, "results": [{"id": format!("{method}{qid}"), "title": "t"}]});
        if let Some(d) = done {
            params["done"] = json!(d);
        }
        json!({"jsonrpc": "2.0", "method": method, "params": params})
    };
    let notes = [
        note("results", 7, Some(false)),
        note("log", 7, Some(true)),
        note("results", 6, Some(true)),
        json!({"jsonrpc": "2.0", "method": "results"}),
        note("results", 7, None),
    ];
    let mut out = vec![];
    assert!(!collect_notes(&notes, 7, &mut out), "only ignored notes said done");
    assert_eq!(out.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["results7", "results7"]);

    let mut out = vec![];
    assert!(collect_notes(&[note("results", 7, Some(true)), note("results", 7, Some(false))], 7, &mut out));
    assert_eq!(out.len(), 2, "done is sticky across one batch");
}

#[test]
fn parse_results_tolerates_bad_replies() {
    assert!(parse_results(&json!({})).is_empty());
    assert!(parse_results(&Value::Null).is_empty());
    assert!(parse_results(&json!({"results": "nope"})).is_empty());
    assert!(parse_results(&json!({"results": [{"title": "no id"}]})).is_empty(), "one bad item drops the batch");
    assert_eq!(parse_results(&json!({"results": [{"id": "1", "title": "t"}]})).len(), 1);
}

#[test]
fn only_timeouts_count_as_timeouts() {
    assert!(Error::Timeout.during("query").is_timeout());
    assert!(!Error::Exited.during("query").is_timeout());
    assert!(!Error::Rpc(json!({"message": "timeout here"})).during("query").is_timeout());
}

/// Run `old` on a second thread, then `new` after a short delay. Returns the old query's
/// results and how long it took, plus the new query's results.
fn supersede(p: &ExternalPlugin, old: &str, new: &str) -> ((Vec<ResultItem>, Duration), Vec<ResultItem>) {
    std::thread::scope(|s| {
        let h = s.spawn(|| {
            let t = Instant::now();
            (ask(p, old), t.elapsed())
        });
        std::thread::sleep(Duration::from_millis(150));
        let newer = ask(p, new);
        (h.join().unwrap(), newer)
    })
}

#[test]
fn a_newer_query_cancels_the_older_one() {
    if !python3() {
        return;
    }
    let (_tmp, m) = marker();
    let p = mock_with(|_| {}, json!({ "marker": m }));
    let first = pid(&p); // qid 1
    let ((old, took), newer) = supersede(&p, "wait", "pid"); // qids 2 and 3
    assert!(old.is_empty());
    assert!(took < Duration::from_millis(1000), "old query gave way at once: {took:?}");
    assert_eq!(newer.len(), 1);
    assert_eq!(newer[0].title, first, "same process");
    assert!(events(&m).iter().any(|e| e.starts_with("cancel 2")), "{:?}", events(&m));
}

#[test]
fn a_busy_plugin_that_ignores_cancel_is_kept() {
    if !python3() {
        return;
    }
    let p = mock();
    let first = pid(&p);
    let ((old, took), newer) = supersede(&p, "sleep:800", "pid");
    assert!(old.is_empty());
    assert!(took < Duration::from_millis(600), "host stopped waiting: {took:?}");
    assert_eq!(newer.len(), 1, "answered once the old work finished");
    assert_eq!(newer[0].title, first, "not restarted");
}

#[test]
fn cancelled_is_neither_a_timeout_nor_a_crash() {
    let e = Error::Cancelled.during("query");
    assert!(e.is_cancelled());
    assert!(!e.is_timeout());
}

/// Collects the output events of one execution.
fn recorder(exec: u64) -> (ExecCtx, Arc<Mutex<Vec<OutputEvent>>>) {
    let got = Arc::new(Mutex::new(vec![]));
    let sink = got.clone();
    (ExecCtx { exec, sink: Arc::new(move |e| lock(&sink).push(e)), input: None }, got)
}

/// Wait until `got` holds a `Done` (or give up after 3 s) and return a copy.
fn until_done(got: &Mutex<Vec<OutputEvent>>) -> Vec<OutputEvent> {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(3) {
        if lock(got).iter().any(|e| matches!(e, OutputEvent::Done(_))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    lock(got).clone()
}

fn start(out: Result<ExecOutcome>) -> OutputStart {
    match out.unwrap() {
        ExecOutcome::Output(o) => *o,
        other => panic!("expected output, got {other:?}"),
    }
}

#[test]
fn static_output_needs_no_stream() {
    if !python3() {
        return;
    }
    let p = mock();
    let (ctx, got) = recorder(7);
    let o = start(p.execute_with("x", "out-static", "", &ctx));
    assert_eq!((o.title.as_str(), o.text.as_str(), o.streaming), ("T", "all of it", false));
    assert_eq!(o.actions.len(), 2);
    assert_eq!(o.actions[1].kind, crate::plugin::ActionKind::Copy);
    assert!(lock(&p.listener).is_none(), "nothing left listening");
    assert!(lock(&got).is_empty());
}

#[test]
fn streamed_output_arrives_in_order_and_stale_or_late_notes_are_dropped() {
    if !python3() {
        return;
    }
    let p = mock();
    let (ctx, got) = recorder(5);
    let o = start(p.execute_with("x", "stream", "", &ctx));
    assert_eq!((o.title.as_str(), o.text.as_str(), o.streaming), ("S", "a", true));
    let events = until_done(&got);
    let more = vec![Action { id: "more".into(), title: "More".into(), ..Default::default() }];
    assert_eq!(
        events,
        [
            OutputEvent::Append("b".into()),
            OutputEvent::Append("c".into()),
            OutputEvent::Actions(more),
            OutputEvent::Done(None)
        ]
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(lock(&got).len(), 4, "nothing after done");
    assert!(lock(&p.listener).is_none());
    assert_eq!(ask(&p, "pid").len(), 1, "queries still work");
}

#[test]
fn output_sent_before_the_reply_is_kept() {
    if !python3() {
        return;
    }
    let p = mock();
    let (ctx, got) = recorder(1);
    assert!(start(p.execute_with("x", "stream-early", "", &ctx)).streaming);
    assert_eq!(
        until_done(&got),
        [OutputEvent::Append("early".into()), OutputEvent::Replace("replaced".into()), OutputEvent::Done(None)]
    );
}

#[test]
fn cancel_stops_the_stream_and_tells_the_plugin() {
    if !python3() {
        return;
    }
    let (_tmp, m) = marker();
    let p = mock_with(|_| {}, json!({ "marker": m }));
    let (ctx, got) = recorder(9);
    start(p.execute_with("x", "stream-forever", "", &ctx));
    std::thread::sleep(Duration::from_millis(100));
    assert!(!lock(&got).is_empty(), "streaming");
    // A query to the same plugin works while it streams.
    assert_eq!(ask(&p, "pid").len(), 1);
    p.reap_idle(); // must not stop a streaming plugin
    p.cancel_output(9);
    let n = lock(&got).len();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(lock(&got).len(), n, "no events after cancel");
    assert!(events(&m).iter().any(|e| e.starts_with("cancel-exec 9")), "{:?}", events(&m));
    p.cancel_output(9); // twice is harmless
}

#[test]
fn reaping_spares_a_streaming_plugin() {
    if !python3() {
        return;
    }
    let p = mock_with(|m| m.run.idle_timeout_s = 0, json!({}));
    let (ctx, _got) = recorder(3);
    start(p.execute_with("x", "stream-forever", "", &ctx));
    let before = pid(&p);
    p.reap_idle();
    assert_eq!(pid(&p), before, "still the same process");
    p.cancel_output(3);
    p.reap_idle();
    assert!(lock(&p.running).is_none(), "reaped once the stream ended");
}

#[test]
fn a_plugin_that_dies_or_fails_ends_the_stream_with_an_error() {
    if !python3() {
        return;
    }
    let p = mock();
    let (ctx, got) = recorder(2);
    start(p.execute_with("x", "stream-crash", "", &ctx));
    let events = until_done(&got);
    assert_eq!(events[0], OutputEvent::Append("x".into()));
    assert!(matches!(events.last(), Some(OutputEvent::Done(Some(_)))), "{events:?}");

    let (ctx, got) = recorder(3);
    start(p.execute_with("x", "stream-err", "", &ctx));
    assert_eq!(until_done(&got), [OutputEvent::Done(Some("quota exceeded".into()))]);
}

#[test]
fn old_style_execute_replies_are_unchanged() {
    if !python3() {
        return;
    }
    let p = mock();
    assert_eq!(p.execute("x", "ok", "").unwrap(), ExecOutcome::Close);
    assert_eq!(p.execute("x", "stay", "").unwrap(), ExecOutcome::KeepOpen);
    assert!(p.execute("x", "fail", "").is_err());
    assert!(lock(&p.listener).is_none());
}

#[test]
fn output_notifications_parse_into_events() {
    assert_eq!(output_events(&json!({"exec": 1})), []);
    assert_eq!(
        output_events(&json!({"exec": 1, "text": "a", "append": "b", "done": true})),
        [OutputEvent::Replace("a".into()), OutputEvent::Append("b".into()), OutputEvent::Done(None)]
    );
    assert_eq!(output_events(&json!({"error": "x"})), [OutputEvent::Done(Some("x".into()))]);
    let input = OutputInput { placeholder: "Ask".into(), action: "f:1".into() };
    assert_eq!(
        output_events(&json!({"input": {"placeholder": "Ask", "action": "f:1"}})),
        [OutputEvent::Input(Some(input.clone()))]
    );
    assert_eq!(output_events(&json!({"input": null})), [OutputEvent::Input(None)], "null withdraws it");
    let out =
        outcome_from_reply(&json!({"ok": true, "output": {"input": {"placeholder": "Ask", "action": "f:1"}}})).unwrap();
    assert!(matches!(out, ExecOutcome::Output(o) if o.input == Some(input)));
    let out = outcome_from_reply(&json!({"ok": true, "output": {"text": "t"}})).unwrap();
    assert!(matches!(out, ExecOutcome::Output(o) if o.text == "t" && !o.streaming && o.title.is_empty()));
    assert_eq!(outcome_from_reply(&json!({"ok": true, "output": "not an object"})).unwrap(), ExecOutcome::Close);
}

#[test]
fn typed_input_reaches_the_plugin() {
    if !python3() {
        return;
    }
    let p = mock();
    let (mut ctx, _) = recorder(4);
    assert_eq!(start(p.execute_with("x", "echo-input", "", &ctx)).text, "<none>", "absent unless typed");
    ctx.input = Some("why?".into());
    assert_eq!(start(p.execute_with("x", "echo-input", "", &ctx)).text, "why?");
}
