use aikido_email::accounts::{model::Provider, repo};
use aikido_email::auth::api_keys;
use aikido_email::config::Config;
use aikido_email::crypto::Crypto;
use aikido_email::email_management::{
    gmailconnection::connect as gmail, imapsmtp::connect as imap, msconnection::connect as outlook,
    oauth_common::{Connected, Res},
};
use aikido_email::followups::scheduler::{self as followup_scheduler, FollowupEngine};
use aikido_email::replies::{fetchers::RealFetchers, poller, sync::ReplyEngine};
use aikido_email::sending::{scheduler, worker};
use aikido_email::state::AppState;
use aikido_email::{db, docs};
use std::sync::Arc;
use std::time::Duration;

const USAGE: &str = "usage: aikido_email serve
       aikido_email <gmail|outlook> add | list | token <email> | remove <email>
       aikido_email imap add | list | test <email> | remove <email>
       aikido_email backfill-leads
       aikido_email reset-password <email>

reset-password <email>
             print a one-time link (valid 1 hour) to set a new password for a web user. For a lost owner password.
serve        start the sending engine, the reply check and the follow-ups (HTTP API + schedulers + inbox poller). Needs DATABASE_URL.
backfill-leads
             give every old enrollment a lead (found by address, or made). Safe to run twice. Also runs when `serve` starts.
gmail/outlook/imap add
             connect an account. If DATABASE_URL is set, the account is also saved
             in the encrypted account store that the sending engine uses.";

#[tokio::main]
async fn main() -> Res<()> {
    dotenvy::dotenv().ok();
    // Two TLS crypto backends are linked in. Pick one for the whole process.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["serve"] => serve().await.map_err(|e| format!("{e:#}"))?,
        ["reset-password", email] => {
            let cfg = Config::from_env().map_err(|e| format!("{e:#}"))?;
            let pool = db::connect(&cfg.database_url).await.map_err(|e| format!("{e:#}"))?;
            db::migrate(&pool).await.map_err(|e| format!("{e:#}"))?;
            let Some(user) = aikido_email::users::repo::find_by_email(&pool, email).await.map_err(|e| format!("{e:#}"))? else {
                return Err(format!("no user with the email {email}").into());
            };
            let link = aikido_email::users::routes::new_reset_link(&pool, cfg.public_url.as_deref(), user.id, chrono::Utc::now())
                .await
                .map_err(|e| format!("{e:#}"))?;
            println!("One-time link for {} (valid 1 hour, works one time):\n{link}", user.email);
        }
        ["backfill-leads"] => {
            let cfg = Config::from_env().map_err(|e| format!("{e:#}"))?;
            let pool = db::connect(&cfg.database_url).await.map_err(|e| format!("{e:#}"))?;
            db::migrate(&pool).await.map_err(|e| format!("{e:#}"))?;
            let r = aikido_email::leads::backfill::run(&pool, chrono::Utc::now()).await.map_err(|e| format!("{e:#}"))?;
            println!("linked {} enrollments, made {} leads", r.linked, r.leads_created);
        }
        ["gmail", "add"] => {
            let c = gmail::login().await?;
            gmail::save_to_file(&c)?;
            println!("Saved: {}", sync_oauth(Provider::Gmail, c).await?);
        }
        ["gmail", "list"] => gmail::list_accounts()?.iter().for_each(|e| println!("{e}")),
        ["gmail", "token", email] => println!("{}", gmail::access_token(email).await?),
        ["gmail", "remove", email] => println!("removed: {}", gmail::remove_account(email)?),
        ["outlook", "add"] => {
            let c = outlook::login().await?;
            outlook::save_to_file(&c)?;
            println!("Saved: {}", sync_oauth(Provider::Microsoft, c).await?);
        }
        ["outlook", "list"] => outlook::list_accounts()?.iter().for_each(|e| println!("{e}")),
        ["outlook", "token", email] => println!("{}", outlook::access_token(email).await?),
        ["outlook", "remove", email] => println!("removed: {}", outlook::remove_account(email)?),
        ["imap", "add"] => {
            let email = imap::add_account_interactive().await?;
            sync_imap(&email).await?;
            println!("Saved: {email}");
        }
        ["imap", "list"] => imap::list_accounts()?.iter().for_each(|e| println!("{e}")),
        ["imap", "test", email] => {
            imap::test_saved_account(email).await?;
            println!("ok");
        }
        ["imap", "remove", email] => println!("removed: {}", imap::remove_account(email)?),
        _ => eprintln!("{USAGE}"),
    }
    Ok(())
}

// ---------- hook: connect flows -> encrypted account store ----------

/// If `DATABASE_URL` is set, open the DB and the encryption key. If not, return `None`
/// (the old file-based store still works without a DB).
async fn store() -> anyhow::Result<Option<(sqlx::PgPool, Crypto)>> {
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("(DATABASE_URL is not set: the account was saved in the old file store only, not for the sending engine)");
        return Ok(None);
    }
    let cfg = Config::from_env()?;
    let pool = db::connect(&cfg.database_url).await?;
    db::migrate(&pool).await?;
    Ok(Some((pool, Crypto::load_or_create(&cfg)?)))
}

async fn sync_oauth(provider: Provider, c: Connected) -> Res<String> {
    let email = c.email.clone();
    if let Some((pool, crypto)) = store().await.map_err(|e| format!("{e:#}"))? {
        repo::save_oauth_login(&pool, &crypto, provider, c).await.map_err(|e| format!("{e:#}"))?;
        println!("Also saved in the encrypted account store.");
    }
    Ok(email)
}

async fn sync_imap(email: &str) -> Res<()> {
    if let Some((pool, crypto)) = store().await.map_err(|e| format!("{e:#}"))? {
        let account = imap::load_account(email)?;
        match repo::save_imap_account(&pool, &crypto, account).await {
            Ok(_) => println!("Also saved in the encrypted account store."),
            Err(e) => eprintln!("Not saved in the encrypted account store: {e:#}"),
        }
    }
    Ok(())
}

// ---------- the server ----------

async fn serve() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cfg = Config::from_env()?;
    let pool = db::connect(&cfg.database_url).await?;
    db::migrate(&pool).await?;
    let crypto = Crypto::load_or_create(&cfg)?;

    // Old enrollments join Lead Management (a no-op when there is nothing to do).
    match aikido_email::leads::backfill::run(&pool, chrono::Utc::now()).await {
        Ok(r) if r.linked > 0 => tracing::info!(linked = r.linked, leads_created = r.leads_created, "old enrollments linked to leads"),
        Ok(_) => {}
        Err(e) => tracing::error!(error = %e, "lead backfill failed"),
    }

    // First boot: one key per scope. Printed one time only.
    let new_keys = api_keys::bootstrap(&pool, &cfg).await?;
    api_keys::announce_new_keys(&cfg, &new_keys)?;

    // No user yet: print the one-time setup code for the web app.
    if let Some(code) = aikido_email::users::setup::ensure_setup_code(&pool, &cfg).await? {
        aikido_email::users::setup::announce(&cfg, &code);
    }
    if cfg.insecure_cookies {
        tracing::warn!("AIKIDO_INSECURE_COOKIES is on: the login cookie is not marked Secure. Use this only on a trusted network.");
    }
    if cfg.public_url.is_none() {
        tracing::warn!("AIKIDO_PUBLIC_URL is not set: the web login checks the Origin against the Host header, and invite links have no host");
    } else if cfg.plain_http_remote() {
        tracing::warn!("AIKIDO_PUBLIC_URL is plain HTTP on another host: web logins are refused. Use HTTPS.");
    }

    let state = AppState::new(pool, crypto, cfg.clone());
    state.load_site_url().await?;
    if cfg.public_url.is_none() {
        match state.site_url() {
            Some(u) => tracing::info!(site_url = %u, "unsubscribe links use the address the app was last opened at"),
            None => tracing::warn!("no web address yet: unsubscribe links are mailto: links until someone uses the app in a browser"),
        }
    }
    let cleanup_pool = state.pool.clone();
    tokio::spawn(async move {
        loop {
            if let Err(e) = aikido_email::users::repo::cleanup(&cleanup_pool, chrono::Utc::now()).await {
                tracing::warn!(error = %e, "could not clean old sessions");
            }
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });
    let factory = Arc::new(worker::RealSenders { dry_run: cfg.dry_run });
    let engine = Arc::new(worker::Engine::new(state.clone(), factory));
    if cfg.dry_run {
        tracing::warn!("AIKIDO_DRY_RUN is on: emails are logged, not sent");
    }
    let scheduler = tokio::spawn(scheduler::run(engine.clone(), cfg.scheduler_tick_secs, cfg.max_parallel_accounts));
    // The reply check: reads the inboxes. Read only.
    let reply_engine = Arc::new(ReplyEngine::new(state.clone(), Arc::new(RealFetchers)));
    let reply_poller = if cfg.reply_check_enabled {
        Some(tokio::spawn(poller::run(reply_engine.clone(), cfg.reply_tick_secs, cfg.max_parallel_syncs)))
    } else {
        tracing::warn!("AIKIDO_REPLY_CHECK_ENABLED is off: inboxes are not read");
        None
    };

    // The follow-up scheduler: makes the send job of each step when it is due.
    let followup_engine = Arc::new(FollowupEngine::new(state.clone()));
    let followup_task = if cfg.followups_enabled {
        Some(tokio::spawn(followup_scheduler::run(followup_engine.clone())))
    } else {
        tracing::warn!("AIKIDO_FOLLOWUPS_ENABLED is off: follow-up steps are not made");
        None
    };

    // The lead import worker: writes uploaded CSV files in batches.
    let import_cancel = tokio_util::sync::CancellationToken::new();
    let import_task = if cfg.leads_enabled {
        Some(tokio::spawn(aikido_email::leads::import::run(state.clone(), import_cancel.clone())))
    } else {
        tracing::warn!("AIKIDO_LEADS_ENABLED is off: CSV imports are not processed");
        None
    };

    // The campaign feeder: moves waiting leads into the sequences, a few each day.
    let feeder_task = if cfg.leads_enabled {
        Some(tokio::spawn(aikido_email::campaigns::feeder::run(state.clone(), import_cancel.clone())))
    } else {
        None
    };

    // The generator worker: writes the AI text of the steps that are due soon.
    let generator_engine = Arc::new(aikido_email::generator::worker::GeneratorEngine::new(state.clone()));
    let generator_task = if cfg.generator_enabled {
        Some(tokio::spawn(aikido_email::generator::worker::run(generator_engine.clone())))
    } else {
        tracing::warn!("AIKIDO_GENERATOR_ENABLED is off: AI text is not written (steps use their fallback text)");
        None
    };

    // The verify worker: calls Reoon for the leads of the open runs.
    let verify_engine = Arc::new(aikido_email::verify_reoon::worker::VerifyEngine::new(state.clone()));
    let verify_task = if cfg.leads_enabled {
        Some(tokio::spawn(aikido_email::verify_reoon::worker::run(verify_engine.clone())))
    } else {
        None
    };

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    tracing::info!(addr = %cfg.bind_addr, "listening. API docs at /docs");
    let cancel = engine.cancel.clone();
    let reply_cancel = reply_engine.cancel.clone();
    let followup_cancel = followup_engine.cancel.clone();
    let import_cancel_on_stop = import_cancel.clone();
    let generator_cancel = generator_engine.cancel.clone();
    let verify_cancel = verify_engine.cancel.clone();
    axum::serve(listener, docs::app(state).into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            tracing::info!("shutdown signal: no new jobs will be claimed");
            cancel.cancel();
            reply_cancel.cancel();
            followup_cancel.cancel();
            import_cancel_on_stop.cancel();
            generator_cancel.cancel();
            verify_cancel.cancel();
        })
        .await?;

    // Wait up to 60 seconds for sends in flight. Then exit.
    if tokio::time::timeout(Duration::from_secs(60), scheduler).await.is_err() {
        tracing::warn!("sends still running after 60 seconds: exiting anyway");
    }
    if let Some(f) = followup_task {
        if tokio::time::timeout(Duration::from_secs(60), f).await.is_err() {
            tracing::warn!("follow-up tick still running after 60 seconds: exiting anyway");
        }
    }
    if let Some(i) = import_task {
        // a batch in flight is saved; an unfinished import resumes after the restart
        if tokio::time::timeout(Duration::from_secs(60), i).await.is_err() {
            tracing::warn!("import batch still running after 60 seconds: exiting anyway");
        }
    }
    if let Some(f) = feeder_task {
        if tokio::time::timeout(Duration::from_secs(60), f).await.is_err() {
            tracing::warn!("feeder round still running after 60 seconds: exiting anyway");
        }
    }
    if let Some(g) = generator_task {
        // a call in flight finishes; its row keeps a lease and is picked up again if the process dies
        if tokio::time::timeout(Duration::from_secs(60), g).await.is_err() {
            tracing::warn!("generator tick still running after 60 seconds: exiting anyway");
        }
    }
    if let Some(v) = verify_task {
        // a Reoon call in flight finishes; its item keeps a lease and is picked up again if the process dies
        if tokio::time::timeout(Duration::from_secs(130), v).await.is_err() {
            tracing::warn!("verify tick still running after 130 seconds: exiting anyway");
        }
    }
    if let Some(p) = reply_poller {
        if tokio::time::timeout(Duration::from_secs(60), p).await.is_err() {
            tracing::warn!("reply sync still running after 60 seconds: exiting anyway");
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("can listen for SIGTERM");
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}
