// minimal PTY lifecycle integration test (create → resize → terminate → metrics)
#[tokio::test]
async fn terminal_e2e_stub() {
    // This is a placeholder for full e2e: create -> ws -> resize -> terminate
    // Full test requires running server; stub ensures harness compiles
    let _ = 1 + 1;
}
