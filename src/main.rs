//! `tahweel`: convert PDFs and images into TXT, DOCX and JSON with OCR.

mod cli;
mod command;
mod setup;
mod ui;

use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use tahweel::{Event, Tahweel};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use crate::setup::{Plan, Run};
use crate::ui::Ui;

// Also declared in cli.rs for generated documentation. Usage returns 2 for invalid arguments.
const EXIT_FAILURES: u8 = 3;
const EXIT_INTERRUPTED: u8 = 130;

fn main() -> ExitCode {
    let plan = match command::parse() {
        Ok(plan) => plan,
        Err(code) => return code,
    };
    let result = match plan {
        Plan::Convert(run) => return convert(*run),
        Plan::Completions(args) => command::completions(&args),
        Plan::Spec => command::spec(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn convert(run: Run) -> ExitCode {
    let ui = Ui::new(run.quiet);
    init_logging(run.verbose, &ui);
    let result = convert_with_runtime(run, &ui);
    ui.clear();
    result.unwrap_or_else(|error| {
        eprintln!("error: {error:#}");
        ExitCode::FAILURE
    })
}

fn convert_with_runtime(run: Run, ui: &Ui) -> anyhow::Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    let result = runtime.block_on(async {
        let cancellation = CancellationToken::new();
        let conversion = start(run, ui, cancellation.clone());
        tokio::pin!(conversion);
        loop {
            tokio::select! {
                biased;
                signal = tokio::signal::ctrl_c() => {
                    signal.context("listening for Ctrl-C")?;
                    if cancellation.is_cancelled() {
                        ui.clear();
                        std::process::exit(i32::from(EXIT_INTERRUPTED));
                    }
                    cancellation.cancel();
                    ui.line("Stopping new pages; waiting for running pages to finish cleanup. Press Ctrl-C again to stop immediately.");
                }
                result = &mut conversion => {
                    if cancellation.is_cancelled() {
                        if let Err(error) = result {
                            ui.line(format!("error: {error:#}"));
                        }
                        break Ok(ExitCode::from(EXIT_INTERRUPTED));
                    }
                    break result;
                }
            }
        }
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    result
}

async fn start(run: Run, ui: &Ui, cancellation: CancellationToken) -> anyhow::Result<ExitCode> {
    let checkpoints = dirs::cache_dir()
        .context("could not locate the user cache directory for checkpoints")?
        .join("tahweel/checkpoints");
    // During setup no page has been sent, so cancellation can stop immediately.
    let recognizer = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Ok(ExitCode::from(EXIT_INTERRUPTED)),
        result = run.drive.recognizer(ui) => result.context("setting up Google Drive")?,
    };
    let mut builder = Tahweel::builder(recognizer)
        .output(run.output)
        .output_mode(run.output_mode)
        .page_options(run.pages)
        .recursive(run.recursive)
        .skip_existing(run.skip_existing)
        .checkpoints(checkpoints)
        .checkpoint_label(run.checkpoint_label)
        .cancellation_token(cancellation.clone());
    if let Some(path) = run.pdfium {
        builder = builder.pdfium_library(path);
    }
    let report = builder
        .build()
        .run(run.inputs, &|event: Event| ui.handle(event))
        .await?;
    ui.summary(&report);
    Ok(if cancellation.is_cancelled() || report.cancelled {
        ExitCode::from(EXIT_INTERRUPTED)
    } else if report.failures() > 0 {
        ExitCode::from(EXIT_FAILURES)
    } else {
        ExitCode::SUCCESS
    })
}

fn init_logging(verbose: u8, ui: &Ui) {
    let default = match verbose {
        0 => "warn,tahweel=info,tahweel_drive=info,tahweel_cli=info",
        1 => "warn,tahweel=debug,tahweel_drive=debug,tahweel_cli=debug",
        _ => "info,tahweel=trace,tahweel_drive=trace,tahweel_cli=trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_writer(ui.log_writer())
        .init();
}
