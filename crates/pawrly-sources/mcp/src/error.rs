/// Runtime failures talking to an MCP server.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("mcp error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("protocol: {0}")]
    Protocol(String),
}

/// Failures registering an MCP source (config + connect + catalog).
#[derive(Debug, thiserror::Error)]
pub enum McpBuildError {
    #[error("config: {0}")]
    Config(String),
    #[error("connect: {0}")]
    Connect(String),
    #[error("datafusion: {0}")]
    DataFusion(String),
}
