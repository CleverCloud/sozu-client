use std::fs;

use sozu_command_lib::{
    config::{ClusterConfig, Config},
    proto::command::request::RequestType,
};

#[test]
fn test_config_2_3_roundtrip_preserves_operational_state() {
    let directory = tempfile::tempdir().expect("temporary directory must be created");
    let path = directory.path().join("sozu.toml");
    fs::write(
        &path,
        r#"
command_socket = "/tmp/sozu.sock"
command_socket_mode = "0660"
saved_state = "./state.json"
worker_count = 1
max_connections_per_subnet = 100
subnet_ipv4_prefix = 24
subnet_ipv6_prefix = 56
retry_after = 30
max_connection_attempts = 5

[[listeners]]
protocol = "http"
address = "127.0.0.1:8080"
interface = "lo"

[clusters.api]
protocol = "http"
load_balancing = "MAGLEV"
max_connections_per_subnet = 7
retry_after = 17
affinity_header = "X-Tenant"
shard_percent = 50
shard_min_backends = 2
shard_mode = "STRICT"
max_connection_attempts = 3
frontends = [
  { address = "127.0.0.1:8080", hostname = "example.com" }
]
backends = [
  { address = "10.0.0.1:8080" },
  { address = "10.0.0.2:8080" }
]

[clusters.api.health_check]
mode = "TCP"
"#,
    )
    .expect("Sōzu configuration fixture must be written");

    let config = sozu_client::config::try_from(&path).expect("Sōzu 2.3 config must load");
    assert_eq!(config.command_socket_mode, 0o660);
    assert_eq!(config.max_connections_per_subnet, 100);
    assert_eq!(config.subnet_ipv4_prefix, 24);
    assert_eq!(config.subnet_ipv6_prefix, 56);
    assert_eq!(config.retry_after, 30);
    assert_eq!(config.max_connection_attempts, 5);

    let encoded = serde_json::to_vec(&config).expect("configuration state must serialize");
    let restored: Config =
        serde_json::from_slice(&encoded).expect("configuration state must deserialize");
    assert!(
        restored == config,
        "configuration state must round-trip exactly"
    );

    let cluster = match restored
        .clusters
        .get("api")
        .expect("API cluster must be present")
    {
        ClusterConfig::Http(cluster) => cluster,
        ClusterConfig::Tcp(_) => panic!("API cluster must use HTTP"),
    };
    assert_eq!(cluster.max_connections_per_subnet, Some(7));
    assert_eq!(cluster.retry_after, Some(17));
    assert_eq!(cluster.affinity_header.as_deref(), Some("X-Tenant"));
    assert_eq!(cluster.shard_percent, Some(50));
    assert_eq!(cluster.shard_min_backends, Some(2));
    assert_eq!(cluster.shard_mode.map(|mode| mode as i32), Some(1));
    assert_eq!(cluster.max_connection_attempts, Some(3));
    assert_eq!(
        cluster.health_check.as_ref().map(|check| check.mode),
        Some(1)
    );

    let add_cluster = restored
        .generate_config_messages()
        .expect("configuration orders must be generated")
        .into_iter()
        .find_map(|request| match request.content.request_type {
            Some(RequestType::AddCluster(cluster)) if cluster.cluster_id == "api" => Some(cluster),
            _ => None,
        })
        .expect("API AddCluster order must be generated");
    assert_eq!(add_cluster.max_connections_per_subnet, Some(7));
    assert_eq!(add_cluster.retry_after, Some(17));
    assert_eq!(add_cluster.affinity_header.as_deref(), Some("X-Tenant"));
    assert_eq!(add_cluster.shard_percent, Some(50));
    assert_eq!(add_cluster.shard_min_backends, Some(2));
    assert_eq!(add_cluster.shard_mode, Some(1));
    assert_eq!(add_cluster.max_connection_attempts, Some(3));
    assert_eq!(add_cluster.health_check.map(|check| check.mode), Some(1));
}
