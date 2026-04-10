pub struct ServerConfig {
    pub otlp_grpc_addr: String,
    pub rest_api_addr: String,
    pub data_dir: String,
}

impl ServerConfig {
    pub fn from_env() -> Self {
        Self {
            otlp_grpc_addr: std::env::var("TAEL_OTLP_GRPC_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:4317".into()),
            rest_api_addr: std::env::var("TAEL_REST_API_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:7701".into()),
            data_dir: std::env::var("TAEL_DATA_DIR")
                .unwrap_or_else(|_| "./data".into()),
        }
    }
}
