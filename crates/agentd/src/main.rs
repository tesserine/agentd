fn main() {
    agentd::configure_tracing().expect("tracing initialization failed");
    println!("agentd v0.1.0");
}
