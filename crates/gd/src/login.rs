//! `gd login`, `gd logout` and `gd whoami`: signing the CLI in with the server's sign-in
//! pages (the OAuth device flow, RFC 8628) and keeping the token in the credentials file.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use gd::credentials;
use gd::remote::GrpcRemote;
use grovedock_proto::{Empty, RevokeTokenRequest};
use serde_json::Value;

/// This machine's name, for naming the token (`cli on HOST`).
fn host_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .or_else(|| {
            let out = std::process::Command::new("hostname").output().ok()?;
            Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        })
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "this machine".into())
}

/// Opens `url` in the default browser, if there is one; failures are fine (the URL is printed).
fn open_browser(url: &str) {
    let cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else if cfg!(windows) {
        std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    let _ = cmd;
}

pub async fn login(server: &str) -> Result<()> {
    let origin = credentials::key(server);
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(format!("gd/{} ({})", env!("CARGO_PKG_VERSION"), std::env::consts::OS))
        .build()?;
    let start = http
        .post(format!("{origin}/login/device/code"))
        .form(&[("client_id", "gd-cli"), ("host", host_name().as_str())])
        .send()
        .await
        .with_context(|| format!("reaching {origin}"))?;
    if start.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("{origin} has no sign-in pages; ask its administrators for a token and set GD_TOKEN");
    }
    let start: Value = start.error_for_status()?.json().await?;
    let (Some(device_code), Some(user_code), Some(uri)) =
        (start["device_code"].as_str(), start["user_code"].as_str(), start["verification_uri"].as_str())
    else {
        bail!("{origin} answered the sign-in request with something unexpected");
    };
    let complete = start["verification_uri_complete"].as_str().unwrap_or(uri);
    eprintln!("To sign in, open {uri} and enter the code {user_code}");
    eprintln!("(opening {complete} in your browser)");
    open_browser(complete);
    let mut interval = start["interval"].as_u64().unwrap_or(5).max(1);
    let token = loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        let r = http
            .post(format!("{origin}/login/device/token"))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code),
                ("client_id", "gd-cli"),
            ])
            .send()
            .await?;
        let v: Value = r.json().await.unwrap_or_default();
        if let Some(t) = v["access_token"].as_str() {
            break t.to_owned();
        }
        match v["error"].as_str() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some("access_denied") => bail!("sign-in was denied in the browser"),
            Some("expired_token") => bail!("the code expired before it was approved; run `gd login` again"),
            other => bail!("sign-in failed: {}", other.unwrap_or("no answer")),
        }
    };
    let path = credentials::path().context("no home directory to keep the token in; set GD_TOKEN instead")?;
    credentials::save(&path, &origin, &token)?;
    let mut p = GrpcRemote::connect(&origin, None, Some(&token))?.platform();
    let me = p.viewer(Empty {}).await.map(|r| r.into_inner().account.map(|a| a.login));
    match me {
        Ok(Some(login)) => println!("signed in to {origin} as {login}"),
        _ => println!("signed in to {origin}"),
    }
    eprintln!("token saved in {}", path.display());
    Ok(())
}

pub async fn logout(server: &str) -> Result<()> {
    let origin = credentials::key(server);
    let path = credentials::path().context("no home directory")?;
    let Some(token) = credentials::load(&path, &origin) else {
        println!("not signed in to {origin}");
        return Ok(());
    };
    // Revoke the token on the server first; forget it here whatever happens.
    let mut p = GrpcRemote::connect(&origin, None, Some(&token))?.platform();
    let revoked = async {
        let id = p.viewer(Empty {}).await?.into_inner().token_id;
        if !id.is_empty() {
            p.revoke_token(RevokeTokenRequest { id }).await?;
        }
        Ok::<_, tonic::Status>(())
    }
    .await;
    credentials::remove(&path, &origin)?;
    match revoked {
        Ok(()) => println!("signed out of {origin}"),
        Err(e) => {
            println!("signed out of {origin} on this machine");
            eprintln!("warning: the server did not revoke the token: {}", e.message());
        }
    }
    Ok(())
}

pub async fn whoami(server: &str, token: Option<&str>) -> Result<()> {
    let mut p = GrpcRemote::connect(server, None, token)?.platform();
    let v = p.viewer(Empty {}).await.map_err(|e| anyhow::anyhow!("{}", e.message()))?.into_inner();
    let a = v.account.context("no account")?;
    let orgs: Vec<String> = v.organizations.into_iter().map(|o| o.login).collect();
    println!("{}{}", a.login, if a.site_admin { " (site administrator)" } else { "" });
    if !orgs.is_empty() {
        println!("organisations: {}", orgs.join(", "));
    }
    Ok(())
}
