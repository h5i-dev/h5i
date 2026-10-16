//! The RealWorld API server; see `conduit_server::serve_from_env` for its
//! environment.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    conduit_server::main_from_env()
}
