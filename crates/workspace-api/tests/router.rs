use loog_workspace_api::*;

fn owner(id: &str, generation: u64) -> Owner {
    Owner {
        id: id.into(),
        generation,
    }
}

#[test]
fn stopped_watches_deliver_only_to_the_owner_and_release_the_handle() {
    let (mut router, provider, a, b, scope) = fixture(RouterLimits::default());
    let id = router
        .submit(&a, scope, Operation::Watch { path: path() }, 0, 100)
        .unwrap();
    router.poll(&provider, 1);
    router
        .complete(&provider, id, Ok(Answer::Started { handle: 7 }))
        .unwrap();
    router.poll(&a, 1);
    assert!(
        router
            .submit(&b, scope, Operation::Unwatch { watch: 7 }, 0, 100)
            .is_err()
    );
    router
        .notify(
            &provider,
            scope,
            Notification::WatchStopped {
                watch: 7,
                error: Failure::new(FailureKind::LimitExceeded, "subscription limit"),
            },
        )
        .unwrap();
    assert!(router.poll(&b, 1).is_empty());
    assert!(matches!(
        &router.poll(&a, 1)[0],
        Delivery::Notification {
            event: Notification::WatchStopped { watch: 7, .. },
            ..
        }
    ));
    assert!(
        router
            .submit(&a, scope, Operation::Unwatch { watch: 7 }, 0, 100)
            .is_err()
    );
    assert!(
        router
            .notify(
                &provider,
                scope,
                Notification::Changed {
                    watch: 7,
                    sequence: 2,
                    paths: vec![path()],
                    rescan: true
                }
            )
            .is_err()
    );
}

#[test]
fn unregistering_one_environment_revokes_its_requests_without_retiring_the_provider() {
    let (mut router, provider, a, _, scope) = fixture(RouterLimits::default());
    let key = EnvironmentKey::new(&provider.id, "test").unwrap();
    let environment = router.resolve(&key).unwrap();
    let another = router
        .register(
            provider.clone(),
            EnvironmentDescriptor {
                key: EnvironmentKey::new(&provider.id, "other").unwrap(),
                name: "Other".into(),
                platform: "test".into(),
                capabilities: vec![Capability::FilesRead],
                badge: None,
            },
        )
        .unwrap();
    let request = router
        .submit(
            &a,
            scope,
            Operation::Write {
                path: path(),
                bytes: vec![1],
                expected_revision: None,
            },
            0,
            100,
        )
        .unwrap();
    router.poll(&provider, 1);
    assert!(router.unregister(&a, environment).is_err());
    router.unregister(&provider, environment).unwrap();
    assert!(router.resolve(&key).is_none());
    assert!(router.environment(another).is_some());
    assert!(matches!(&router.poll(&a, 1)[0], Delivery::Complete {
        request: id, result: Err(Failure { kind: FailureKind::UnknownOutcome, .. })
    } if *id == request));
    assert!(
        router
            .submit(&a, scope, Operation::Stat { path: path() }, 0, 100)
            .is_err()
    );
}
fn path() -> ProviderPath {
    ProviderPath::new("/project").unwrap()
}

#[test]
fn execution_revocation_and_boundary_changes_preserve_file_subscriptions() {
    let (mut router, provider, consumer, _, scope) = fixture(RouterLimits::default());
    let spawn = || Operation::Spawn {
        program: "tool".into(),
        args: vec![],
        cwd: path(),
        env: vec![],
    };
    for (operation, handle) in [(Operation::Watch { path: path() }, 7), (spawn(), 8)] {
        let id = router.submit(&consumer, scope, operation, 0, 100).unwrap();
        router.poll(&provider, 1);
        router
            .complete(&provider, id, Ok(Answer::Started { handle }))
            .unwrap();
        router.poll(&consumer, 1);
    }
    router
        .grant(
            consumer.clone(),
            scope,
            [Capability::FilesRead, Capability::Watch],
        )
        .unwrap();
    assert!(matches!(
        &router.poll(&provider, 10)[..],
        [Delivery::Release {
            capability: Capability::Process,
            handle: 8,
            ..
        }]
    ));
    assert!(matches!(
        &router.poll(&consumer, 10)[..],
        [Delivery::Notification {
            event: Notification::Exited {
                handle: 8,
                code: None
            },
            ..
        }]
    ));
    assert_eq!(
        router
            .submit(&consumer, scope, spawn(), 0, 100)
            .unwrap_err()
            .kind,
        FailureKind::PermissionDenied
    );
    router
        .grant(
            consumer.clone(),
            scope,
            [
                Capability::FilesRead,
                Capability::Watch,
                Capability::Process,
            ],
        )
        .unwrap();
    assert!(router.poll(&provider, 10).is_empty());
    let excluded = ProviderPath::new("/project/Nested\\opaque").unwrap();
    router
        .restrict_execution(scope, vec![excluded.clone()])
        .unwrap();
    let id = router.submit(&consumer, scope, spawn(), 0, 100).unwrap();
    assert!(
        matches!(&router.poll(&provider, 1)[0], Delivery::Request(request) if request.excluded_roots == vec![excluded.clone()])
    );
    router
        .complete(&provider, id, Ok(Answer::Started { handle: 9 }))
        .unwrap();
    router.poll(&consumer, 1);
    router.restrict_execution(scope, vec![]).unwrap();
    assert!(matches!(
        &router.poll(&provider, 10)[..],
        [Delivery::Release {
            capability: Capability::Process,
            handle: 9,
            ..
        }]
    ));
    router.poll(&consumer, 10);
    router
        .notify(
            &provider,
            scope,
            Notification::Changed {
                watch: 7,
                sequence: 1,
                paths: vec![path()],
                rescan: false,
            },
        )
        .unwrap();
    assert!(matches!(
        &router.poll(&consumer, 10)[..],
        [Delivery::Notification {
            event: Notification::Changed { watch: 7, .. },
            ..
        }]
    ));
    assert!(
        router
            .submit(&consumer, scope, Operation::Unwatch { watch: 7 }, 0, 100)
            .is_ok()
    );
}
#[test]
fn a_badge_is_short_plain_text_and_is_kept_with_the_environment() {
    let provider = owner("example.provider", 1);
    let register = |router: &mut Router, name: &str, badge: Option<String>| {
        router.register(
            provider.clone(),
            EnvironmentDescriptor {
                key: EnvironmentKey::new(&provider.id, name).unwrap(),
                name: "Test".into(),
                platform: "test".into(),
                capabilities: vec![Capability::FilesRead],
                badge,
            },
        )
    };
    let mut router = Router::new(RouterLimits::default());
    let id = register(&mut router, "one", Some("web-01".into())).unwrap();
    assert_eq!(router.environment(id).unwrap().badge.as_deref(), Some("web-01"));
    assert!(register(&mut router, "two", None).is_ok(), "a badge is optional");
    for bad in [
        String::new(),
        "x".repeat(MAX_BADGE_BYTES + 1),
        "a\nb".into(),
    ] {
        assert!(register(&mut router, "bad", Some(bad)).is_err());
    }
}

fn fixture(limits: RouterLimits) -> (Router, Owner, Owner, Owner, ScopeId) {
    let mut router = Router::new(limits);
    let provider = owner("example.provider", 1);
    let a = owner("consumer.a", 1);
    let b = owner("consumer.b", 1);
    let caps = vec![
        Capability::FilesRead,
        Capability::FilesWrite,
        Capability::Process,
        Capability::Watch,
        Capability::Search,
    ];
    let env = router
        .register(
            provider.clone(),
            EnvironmentDescriptor {
                key: EnvironmentKey::new(&provider.id, "test").unwrap(),
                name: "Test".into(),
                platform: "test".into(),
                capabilities: caps.clone(),
                badge: None,
            },
        )
        .unwrap();
    let scope = router.mount(&provider, env, path()).unwrap();
    router.grant(a.clone(), scope, caps.clone()).unwrap();
    router.grant(b.clone(), scope, caps).unwrap();
    (router, provider, a, b, scope)
}

#[test]
fn search_pages_and_cleanup_are_owned_and_consumed_pages_are_not_replayed() {
    let (mut router, provider, a, b, scope) = fixture(RouterLimits::default());
    let options = serde_json::from_value(serde_json::json!({
        "spec": {"query":"needle","include":"","exclude":"","case_sensitive":true,
            "whole_word":false,"regex":false,"include_hidden":false,"include_ignored":false},
        "encoding": {}, "buffers": []
    }))
    .unwrap();
    let start = router
        .submit(
            &a,
            scope,
            Operation::Search {
                path: path(),
                options,
            },
            0,
            100,
        )
        .unwrap();
    router.poll(&provider, 1);
    router
        .complete(&provider, start, Ok(Answer::Started { handle: 71 }))
        .unwrap();
    router.poll(&a, 1);
    let next = || Operation::SearchNext {
        search: 71,
        limit: 16,
    };
    assert_eq!(
        router.submit(&b, scope, next(), 0, 100).unwrap_err().kind,
        FailureKind::PermissionDenied
    );
    let page = router.submit(&a, scope, next(), 0, 100).unwrap();
    router.poll(&provider, 1);
    router.cancel(&a, page).unwrap();
    assert!(matches!(
        &router.poll(&a, 1)[0],
        Delivery::Complete {
            result: Err(Failure {
                kind: FailureKind::UnknownOutcome,
                ..
            }),
            ..
        }
    ));
    router.poll(&provider, 10);
    let close = router
        .submit(&a, scope, Operation::SearchClose { search: 71 }, 0, 100)
        .unwrap();
    router.poll(&provider, 1);
    router.complete(&provider, close, Ok(Answer::Ok)).unwrap();
    assert_eq!(
        router.submit(&a, scope, next(), 0, 100).unwrap_err().kind,
        FailureKind::PermissionDenied
    );
}

#[test]
fn requests_and_results_arrive_on_separate_turns_and_old_generations_cannot_answer() {
    let (mut router, provider, a, _, scope) = fixture(RouterLimits::default());
    let id = router
        .submit(
            &a,
            scope,
            Operation::Read {
                path: path(),
                offset: 0,
                limit: 32,
            },
            0,
            50,
        )
        .unwrap();
    assert!(router.poll(&a, 10).is_empty());
    assert!(router.complete(&provider, id, Ok(Answer::Ok)).is_err());
    assert!(matches!(&router.poll(&provider, 1)[0], Delivery::Request(r) if r.id == id));
    let answer = Answer::Bytes {
        bytes: b"hello".to_vec(),
        revision: Some("1".into()),
        eof: true,
    };
    assert_eq!(
        router
            .complete(&owner(&provider.id, 2), id, Ok(answer.clone()))
            .unwrap_err()
            .kind,
        FailureKind::PermissionDenied
    );
    router.complete(&provider, id, Ok(answer)).unwrap();
    assert_eq!(router.pending_count(), 0);
    assert!(
        matches!(&router.poll(&a, 1)[0], Delivery::Complete { request, result: Ok(Answer::Bytes { bytes, .. }) } if *request == id && bytes == b"hello")
    );
    assert!(router.complete(&provider, id, Ok(Answer::Ok)).is_err());
    assert_eq!(router.queued_bytes(), 0);
}

#[test]
fn cancellation_before_delivery_prevents_execution_and_after_write_reports_unknown() {
    let (mut router, provider, a, _, scope) = fixture(RouterLimits::default());
    let write = || Operation::Write {
        path: path(),
        bytes: vec![7],
        expected_revision: Some("1".into()),
    };
    let first = router.submit(&a, scope, write(), 0, 100).unwrap();
    router.cancel(&a, first).unwrap();
    assert!(router.poll(&provider, 10).is_empty());
    assert!(matches!(
        &router.poll(&a, 10)[0],
        Delivery::Complete {
            result: Err(Failure {
                kind: FailureKind::Cancelled,
                ..
            }),
            ..
        }
    ));
    let second = router.submit(&a, scope, write(), 0, 100).unwrap();
    router.poll(&provider, 10);
    router.cancel(&a, second).unwrap();
    assert!(matches!(
        &router.poll(&a, 10)[0],
        Delivery::Complete {
            result: Err(Failure {
                kind: FailureKind::UnknownOutcome,
                ..
            }),
            ..
        }
    ));
    assert!(
        matches!(&router.poll(&provider, 10)[0], Delivery::Cancel { request } if *request == second)
    );
}

#[test]
fn process_handles_and_output_belong_to_the_requesting_consumer() {
    let (mut router, provider, a, b, scope) = fixture(RouterLimits::default());
    let id = router
        .submit(
            &a,
            scope,
            Operation::Spawn {
                program: "tool".into(),
                args: vec![],
                cwd: path(),
                env: vec![],
            },
            0,
            100,
        )
        .unwrap();
    router.poll(&provider, 10);
    router
        .complete(&provider, id, Ok(Answer::Started { handle: 17 }))
        .unwrap();
    router.poll(&a, 10);
    assert_eq!(
        router
            .submit(&b, scope, Operation::Kill { process: 17 }, 0, 100)
            .unwrap_err()
            .kind,
        FailureKind::PermissionDenied
    );
    router
        .notify(
            &provider,
            scope,
            Notification::Output {
                handle: 17,
                bytes: b"private".to_vec(),
                stderr: false,
            },
        )
        .unwrap();
    assert!(router.poll(&b, 10).is_empty());
    assert!(
        matches!(&router.poll(&a, 10)[0], Delivery::Notification { event: Notification::Output { bytes, .. }, .. } if bytes == b"private")
    );
    router
        .notify(
            &provider,
            scope,
            Notification::Exited {
                handle: 17,
                code: Some(0),
            },
        )
        .unwrap();
    assert!(
        router
            .submit(&a, scope, Operation::Kill { process: 17 }, 0, 100)
            .is_err()
    );
}

#[test]
fn full_data_queues_do_not_block_cancel_and_backpressure_does_not_consume_request_ids() {
    let limits = RouterLimits {
        events: 1,
        ..Default::default()
    };
    let (mut router, provider, a, _, scope) = fixture(limits);
    let read = || Operation::Read {
        path: path(),
        offset: 0,
        limit: 32,
    };
    let id = router.submit(&a, scope, read(), 0, 100).unwrap();
    assert_eq!(
        router.submit(&a, scope, read(), 0, 100).unwrap_err().kind,
        FailureKind::LimitExceeded
    );
    router.cancel(&a, id).unwrap();
    assert!(router.poll(&provider, 10).is_empty());
    router.poll(&a, 10);
    assert_eq!(router.submit(&a, scope, read(), 0, 100).unwrap(), id + 1);
}

#[test]
fn retiring_a_provider_invalidates_scopes_and_preserves_unknown_mutation_outcomes() {
    let (mut router, provider, a, _, scope) = fixture(RouterLimits::default());
    router
        .submit(
            &a,
            scope,
            Operation::Remove {
                path: path(),
                recursive: false,
            },
            0,
            100,
        )
        .unwrap();
    router.poll(&provider, 10);
    router.retire(&provider);
    assert!(matches!(
        &router.poll(&a, 10)[0],
        Delivery::Complete {
            result: Err(Failure {
                kind: FailureKind::UnknownOutcome,
                ..
            }),
            ..
        }
    ));
    assert!(
        router
            .submit(&a, scope, Operation::Stat { path: path() }, 0, 100)
            .is_err()
    );
    assert_eq!(router.pending_count(), 0);
    assert_eq!(router.queued_bytes(), 0);
}

#[test]
fn oversized_answers_are_refused_atomically_and_can_be_replaced_with_a_failure() {
    let (mut router, provider, a, _, scope) = fixture(RouterLimits {
        payload_bytes: 512,
        ..Default::default()
    });
    let id = router
        .submit(
            &a,
            scope,
            Operation::Read {
                path: path(),
                offset: 0,
                limit: 32,
            },
            0,
            100,
        )
        .unwrap();
    router.poll(&provider, 1);
    assert_eq!(
        router
            .complete(
                &provider,
                id,
                Ok(Answer::Bytes {
                    bytes: vec![255; 1024],
                    revision: None,
                    eof: true
                })
            )
            .unwrap_err()
            .kind,
        FailureKind::LimitExceeded
    );
    assert_eq!(router.pending_count(), 1);
    assert!(router.poll(&a, 10).is_empty());
    router
        .complete(
            &provider,
            id,
            Err(Failure::new(FailureKind::Failed, "read failed")),
        )
        .unwrap();
    assert_eq!(router.pending_count(), 0);
}

#[test]
fn deadline_expiry_and_consumer_retirement_release_pending_work() {
    let (mut router, provider, a, _, scope) = fixture(RouterLimits::default());
    router
        .submit(&a, scope, Operation::Stat { path: path() }, 10, 20)
        .unwrap();
    router.expire(29).unwrap();
    assert_eq!(router.pending_count(), 1);
    router.expire(30).unwrap();
    assert!(matches!(
        &router.poll(&a, 10)[0],
        Delivery::Complete {
            result: Err(Failure {
                kind: FailureKind::DeadlineExceeded,
                ..
            }),
            ..
        }
    ));
    assert!(router.poll(&provider, 10).is_empty());
    router
        .submit(&a, scope, Operation::Stat { path: path() }, 40, 20)
        .unwrap();
    router.retire(&a);
    assert_eq!(router.pending_count(), 0);
    assert!(router.poll(&a, 10).is_empty());
}
