use super::*;

#[tokio::test]
async fn interrupted_owner_closes_isolated_sqlite_session() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("isolated.db");
    let config = crate::config::parse(
        &json!({
            "connections":[{"alias":"test","driver_id":"sqlite","host":database,"enabled":true,"properties":{"read_only":"false"}}]
        })
        .to_string(),
    )
    .unwrap()
    .validate()
    .unwrap();
    let params = config
        .connection("test")
        .unwrap()
        .to_connection_params()
        .unwrap();
    let sessions = sqarvo_connection::SessionManager::new(Arc::new(
        sqarvo_drivers::DriverRegistry::with_builtin_drivers(),
    ));
    let handle = sessions
        .connect_isolated("first", params.clone())
        .await
        .unwrap();
    assert_eq!(sessions.active_count().await, 0);
    let (ready, wait) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let session = Disposable {
            handle: Some(handle),
            reservation: None,
        };
        let handle = session.handle.as_ref().unwrap();
        write_transaction::begin(
            handle,
            false,
            Instant::now() + BUDGET,
            std::future::pending(),
        )
        .await
        .unwrap();
        ready.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    wait.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());

    let handle = sessions.connect_isolated("second", params).await.unwrap();
    let tx = write_transaction::begin(
        &handle,
        false,
        Instant::now() + BUDGET,
        std::future::pending(),
    )
    .await
    .unwrap();
    handle
        .rollback_isolated_transaction(&tx.transaction_id)
        .await
        .unwrap();
    handle.close().await.unwrap();
}

#[tokio::test]
async fn isolated_close_failure_attempts_abort_and_preserves_warning() {
    let mut aborted = false;
    let closed = close_connection(
        std::future::ready(Err(Error::internal("injected shutdown error"))),
        async {
            aborted = true;
            true
        },
    )
    .await;
    assert!(aborted);
    assert!(!closed);
}
