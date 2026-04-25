use daemon::Daemon;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_pushes_initial_state_and_accepts_retry() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let ws_root = tempfile::tempdir().unwrap();
    let mut d = Daemon::new(ws_root.path().to_path_buf());
    d.idle_timeout = std::time::Duration::from_secs(3);
    let task = tokio::spawn(async move { let _ = d.run().await; });
    let mut disc = None;
    for _ in 0..50 {
        if let Ok(Some(x)) = daemon::discovery::read_discovery(ws_root.path()) {
            disc = Some(x);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let port = disc.unwrap().http_port;
    let url = format!("ws://127.0.0.1:{port}/ws");
    let (mut socket, _) = connect_async(url).await.unwrap();
    let first = socket.next().await.unwrap().unwrap();
    let txt = match first {
        Message::Text(t) => t,
        _ => panic!("expected Text, got {first:?}"),
    };
    assert!(txt.contains("\"state\""));
    socket
        .send(Message::Text(
            "{\"type\":\"RetryTask\",\"package\":\"x\",\"script\":\"build\"}".to_string(),
        ))
        .await
        .unwrap();
    let next =
        tokio::time::timeout(std::time::Duration::from_secs(2), socket.next()).await;
    assert!(next.is_ok());
    task.abort();
}
