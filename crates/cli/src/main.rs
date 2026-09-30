use std::process::ExitCode;

const USAGE: &str = "usage: reqlite send <request.toml>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [cmd, path] = args.as_slice() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if cmd != "send" {
        eprintln!("unknown command {cmd:?}\n{USAGE}");
        return ExitCode::from(2);
    }
    match run(path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let req = reqlite_format::parse(&text)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let resp = rt.block_on(async {
        let client = reqlite_engine::client()?;
        Ok::<_, Box<dyn std::error::Error>>(reqlite_engine::send(&client, &req).await?)
    })?;

    eprintln!(
        "{} · {} ms · {} bytes",
        resp.status,
        resp.elapsed.as_millis(),
        resp.body.len()
    );
    for (k, v) in &resp.headers {
        eprintln!("{k}: {v}");
    }
    println!("{}", String::from_utf8_lossy(&resp.body));
    Ok(())
}
