pub fn is_network_fixture(file_name: &str) -> bool {
    matches!(
        file_name,
        "test_std_http.mux"
            | "test_std_http_server.mux"
            | "test_std_local_net.mux"
            | "test_std_tcp.mux"
            | "test_std_udp.mux"
    )
}
