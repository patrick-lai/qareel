fn main() {
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error[qareel.runtime]: could not start: {error}");
            std::process::exit(1);
        }
    };
    let code = runtime.block_on(qareel::main(std::env::args().skip(1).collect()));
    std::process::exit(code);
}
