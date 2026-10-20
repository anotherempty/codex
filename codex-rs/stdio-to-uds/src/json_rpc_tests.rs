use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::io::DuplexStream;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tokio::time::timeout;

async fn connection() -> (
    DuplexStream,
    BufReader<DuplexStream>,
    WebSocketStream<DuplexStream>,
    JoinHandle<anyhow::Result<()>>,
) {
    let (client, server) = tokio::io::duplex(8192);
    let (client, server) = tokio::join!(
        tokio_tungstenite::client_async("ws://localhost/", client),
        tokio_tungstenite::accept_async(server),
    );
    let (input, stdin) = tokio::io::duplex(8192);
    let (stdout, output) = tokio::io::duplex(8192);
    let task = tokio::spawn(relay(stdin, stdout, client.unwrap().0));
    (input, BufReader::new(output), server.unwrap(), task)
}

#[tokio::test]
async fn forwards_requests_notifications_and_approval_responses() {
    let (mut input, mut output, mut server, task) = connection().await;
    let request = json!({"id": "initialize-1", "method": "initialize", "params": {"clientInfo": {"name": "ide", "version": "1"}}});
    input
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let message = server.next().await.unwrap().unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(message.to_text().unwrap()).unwrap(),
        request
    );
    // Pretty-printed server messages must still occupy exactly one stdio line.
    let approval = json!({"id": 42, "method": "item/commandExecution/requestApproval", "params": {"command": "echo test"}});
    server
        .send(Message::Text(
            serde_json::to_string_pretty(&approval).unwrap().into(),
        ))
        .await
        .unwrap();
    let mut line = String::new();
    output.read_line(&mut line).await.unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap(),
        approval
    );
    let answer = json!({"id": 42, "result": {"decision": "accept"}});
    input
        .write_all(format!("{answer}\n").as_bytes())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            server.next().await.unwrap().unwrap().to_text().unwrap()
        )
        .unwrap(),
        answer
    );
    let notification = json!({"method": "turn/completed", "params": {"threadId": "same-thread"}});
    server
        .send(Message::Text(notification.to_string().into()))
        .await
        .unwrap();
    line.clear();
    output.read_line(&mut line).await.unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap(),
        notification
    );
    input.shutdown().await.unwrap();
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        server.next().await.unwrap().unwrap(),
        Message::Close(_)
    ));
}

#[tokio::test]
async fn answers_ping_without_client_traffic() {
    let (mut input, _output, mut server, task) = connection().await;
    server
        .send(Message::Ping(vec![1, 2, 3].into()))
        .await
        .unwrap();
    let pong = timeout(Duration::from_secs(1), server.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(pong, Message::Pong(vec![1, 2, 3].into()));
    input.shutdown().await.unwrap();
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn server_close_exits_even_with_client_input_open() {
    let (_input, _output, mut server, task) = connection().await;
    server.close(None).await.unwrap();
    let error = timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("shared app-server disconnected"));
}

#[tokio::test]
async fn rejects_invalid_client_json() {
    let (mut input, _output, _server, task) = connection().await;
    input.write_all(b"not-json\n").await.unwrap();
    let error = timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("invalid JSON on client stdin"));
}

#[tokio::test]
async fn simultaneous_large_messages_do_not_deadlock() {
    let (mut input, mut output, mut server, task) = connection().await;
    let request = json!({"id": 1, "method": "test", "params": "x".repeat(65536)});
    let notification = json!({"method": "test/event", "params": "y".repeat(65536)});
    timeout(Duration::from_secs(2), async {
        let send_input = async {
            input
                .write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
        };
        let read_output = async {
            let mut line = String::new();
            output.read_line(&mut line).await.unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&line).unwrap(),
                notification
            );
        };
        let backend = async {
            // Both messages exceed the socket capacity. Neither peer can finish
            // writing until the other independently makes progress reading.
            server
                .send(Message::Text(notification.to_string().into()))
                .await
                .unwrap();
            let message = server.next().await.unwrap().unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(message.to_text().unwrap()).unwrap(),
                request
            );
        };
        tokio::join!(send_input, read_output, backend);
    })
    .await
    .unwrap();
    input.shutdown().await.unwrap();
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn rejects_binary_server_messages() {
    let (_input, _output, mut server, task) = connection().await;
    server
        .send(Message::Binary(vec![0, 1, 2].into()))
        .await
        .unwrap();
    let error = timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("unexpected non-text"));
}
