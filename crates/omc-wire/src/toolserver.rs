//! The tool servers a conversation is given.

/// How a tool server is reached.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Reach {
    /// A program the harness starts and speaks to on its standard streams.
    Command {
        /// The program.
        program: String,
        /// Its arguments.
        args: Vec<String>,
        /// The environment it is started with, over the harness's own.
        env: Vec<(String, String)>,
    },
    /// An HTTP address the harness connects to.
    Http {
        /// The address.
        url: String,
        /// The headers every request is sent with.
        headers: Vec<(String, String)>,
    },
}

/// One tool server, by the name its tools are filed under.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolServer {
    /// What the server is called.
    pub name: String,
    /// How it is reached.
    pub reach: Reach,
}
