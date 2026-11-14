use super::*;
use crate::app::resource_process;
use loog_workspace_api::FailureKind;

pub(super) fn worker<T: Send + 'static>(
    ui: &SavingUi,
    work: impl FnOnce() -> T + Send + 'static,
) -> T {
    let result = Arc::new(Mutex::new(None));
    let sent = result.clone();
    let thread = std::thread::spawn(move || *sent.lock().unwrap() = Some(work()));
    wait(ui, |_| result.lock().unwrap().is_some());
    thread.join().unwrap();
    result.lock().unwrap().take().unwrap()
}

pub(super) fn run(ui: &SavingUi, host: &ExtensionHost, root: &loog_workspace_api::PresentedRoot) {
    println!(
        "CHECK: native bounded process capture, bidirectional pipes, timeout and execution grants"
    );
    ui.runtime
        .trust()
        .decide_resource(&root.resource, true)
        .unwrap();
    host.refresh_resource_grants();
    let input: Vec<_> = (0..700_000).map(|n| (n % 256) as u8).collect();
    let context = ui.state.clone();
    let cwd = root.resource.clone();
    let scope = root.scope;
    let sent = input.clone();
    let output = worker(ui, move || resource_process::run(&context, scope, resource_process::Command {
        program: "python3".into(),
        args: vec!["-c".into(), "import sys; sys.stdout.buffer.write(b'x'*300000); sys.stdout.buffer.flush(); data=sys.stdin.buffer.read(); sys.stdout.buffer.write(data); sys.stderr.buffer.write(bytes([255,0])*1000); sys.exit(7)".into()],
        cwd, env: vec![], input: sent, timeout: Duration::from_secs(30),
    }, &|| false)).unwrap();
    assert_eq!(output.code, Some(7));
    assert_eq!(&output.stdout[..300_000], vec![b'x'; 300_000]);
    assert_eq!(&output.stdout[300_000..], input);
    assert_eq!(output.stderr, [255, 0].repeat(1000));

    // Exercise EOF racing an already-exited, non-reading child repeatedly.
    for iteration in 0..20 {
        let context = ui.state.clone();
        let cwd = root.resource.clone();
        let output = worker(ui, move || {
            resource_process::run(
                &context,
                scope,
                resource_process::Command {
                    program: "python3".into(),
                    args: vec![
                        "-c".into(),
                        "import sys; sys.stdout.buffer.write(b'done'); sys.exit(3)".into(),
                    ],
                    cwd,
                    env: vec![],
                    input: vec![b'x'; if iteration % 2 == 0 { 700_000 } else { 0 }],
                    timeout: Duration::from_secs(10),
                },
                &|| false,
            )
        })
        .unwrap();
        assert_eq!(output.code, Some(3));
        assert_eq!(output.stdout, b"done");
    }

    let context = ui.state.clone();
    let cwd = root.resource.clone();
    let error = worker(ui, move || {
        resource_process::run(
            &context,
            scope,
            resource_process::Command {
                program: "python3".into(),
                args: vec!["-c".into(), "import time; time.sleep(60)".into()],
                cwd,
                env: vec![],
                input: vec![],
                timeout: Duration::from_secs(2),
            },
            &|| false,
        )
    })
    .unwrap_err();
    assert_eq!(error.kind, FailureKind::UnknownOutcome);

    ui.runtime
        .trust()
        .decide_resource(&root.resource, false)
        .unwrap();
    host.refresh_resource_grants();
    let context = ui.state.clone();
    let cwd = root.resource.clone();
    let error = worker(ui, move || {
        resource_process::run(
            &context,
            scope,
            resource_process::Command {
                program: "python3".into(),
                args: vec!["-c".into(), "raise AssertionError('must not run')".into()],
                cwd,
                env: vec![],
                input: vec![],
                timeout: Duration::from_secs(5),
            },
            &|| false,
        )
    })
    .unwrap_err();
    assert_eq!(error.kind, FailureKind::PermissionDenied);
}
