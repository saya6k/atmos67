//! `atmos67 calibrate`: room correction measured through the running sender
//! and designed by SECS 2R (tasks/dsp.md D8).
//!
//! The work is split into steps so a user interface can ask for the
//! microphone to be moved between positions; the SECS project file carries
//! the state from one step to the next.
//!
//! - `start` makes the project: the speakers the sender sends (its mains,
//!   then its subwoofers), the microphone and its calibration file.
//! - `measure` measures one position.
//! - `finish` designs the correction and exports it as a folder for `--dsp`.
//!
//! The sender must already be running the same topology, which a manual
//! design with the same subwoofer count gives it. Progress goes to standard
//! output as one JSON object per line.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use secs::types::ipc::{
    CalibrateParams, ConfigIndexParams, ConfigSubCountParams, ExportKind, ExportParams,
    JobEvent, MeasureConfigureParams, MeasurePositionParams, MicCalibrationSetParams, ProjectNewParams,
    ProjectPathParams, ProjectSaveParams,
};
use secs::{Engine, EngineBuilder, Request, Response, Secs};

use crate::measure::{Backend, OUTPUT_ID};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Longest a measuring or design step may take.
const JOB_TIMEOUT: Duration = Duration::from_secs(4 * 3600);

pub struct Options {
    /// Folder of the SECS project.
    pub project: PathBuf,
    /// The sender's control socket.
    pub control: PathBuf,
    /// ALSA capture device of the microphone.
    pub microphone: String,
    /// Test replacement for `arecord`.
    pub capture_command: Option<Vec<String>>,
}

fn progress(step: &str, detail: &str) {
    println!("{}", serde_json::json!({ "step": step, "detail": detail }));
}

fn project_file(options: &Options) -> PathBuf {
    options.project.join("room.secs")
}

fn engine(options: &Options, outputs: usize) -> Engine {
    let backend = Backend {
        control: options.control.clone(),
        outputs: outputs as u16,
        microphone: options.microphone.clone(),
        microphone_channels: 1,
        capture_command: options.capture_command.clone(),
    };
    EngineBuilder::default()
        .audio_backend_factory(Arc::new(move || Box::new(backend.clone())))
        .build()
}

fn call(engine: &Engine, request: Request) -> Result<Response> {
    Ok(engine.call(request).map_err(|e| format!("SECS 2R: {e}"))?)
}

/// Runs a request that starts a job and waits for the job to end.
fn job(engine: &Engine, request: Request) -> Result<()> {
    let job = match call(engine, request)? {
        Response::MeasurePosition(started) | Response::Calibrate(started) | Response::Export(started) => started.job,
        other => return Err(format!("unexpected SECS 2R response {other:?}").into()),
    };
    match engine.wait_job(job, JOB_TIMEOUT).map_err(|e| format!("SECS 2R: {e}"))? {
        JobEvent::Finished { .. } => Ok(()),
        other => Err(format!("SECS 2R job ended with {other:?}").into()),
    }
}

/// The sender's output roles, kept beside the project by `start`.
fn outputs(options: &Options) -> Result<Vec<String>> {
    Ok(serde_json::from_slice(&fs::read(options.project.join("outputs.json"))?)?)
}

/// Makes the SECS project for the sender's `outputs` (mains, then SW1…).
pub fn start(options: &Options, outputs: &[String], microphone_calibration: Option<&Path>) -> Result<()> {
    fs::create_dir_all(&options.project)?;
    fs::write(options.project.join("outputs.json"), serde_json::to_vec(outputs)?)?;
    let mains: Vec<&String> = outputs.iter().filter(|r| !r.starts_with("SW")).collect();
    let subs = outputs.len() - mains.len();
    let engine = engine(options, outputs.len());
    // The broadest preset, trimmed to the mains the sender has.
    call(
        &engine,
        Request::ProjectNew(ProjectNewParams {
            path: project_file(options).display().to_string(),
            preset: "9.1.6".into(),
            sub_count: subs as u8,
            sample: false,
            tutorial: false,
        }),
    )?;
    let Response::ConfigGet(config) = call(&engine, Request::ConfigGet)? else {
        return Err("unexpected SECS 2R response to ConfigGet".into());
    };
    for (index, speaker) in config.speakers.iter().enumerate().rev() {
        if !speaker.role.starts_with("SW") && !mains.contains(&&speaker.role) {
            call(&engine, Request::ConfigRemoveSpeaker(ConfigIndexParams { index }))?;
        }
    }
    let Response::ConfigGet(config) = call(&engine, Request::ConfigGet)? else {
        return Err("unexpected SECS 2R response to ConfigGet".into());
    };
    for role in &mains {
        if !config.speakers.iter().any(|s| &&s.role == role) {
            return Err(format!("SECS 2R has no speaker role {role}").into());
        }
    }
    call(&engine, Request::ConfigSetSubCount(ConfigSubCountParams { count: subs as i64, confirm: true }))?;
    call(&engine, Request::ConfigRenumber)?;

    let Response::MeasureConfigGet(plan) = call(&engine, Request::MeasureConfigGet)? else {
        return Err("unexpected SECS 2R response to MeasureConfigGet".into());
    };
    let output_channels = plan
        .roles
        .iter()
        .map(|role| {
            outputs
                .iter()
                .position(|o| o == role)
                .map(|i| i as u16 + 1)
                .ok_or_else(|| format!("the sender has no output for {role}"))
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    call(
        &engine,
        Request::MeasureConfigure(MeasureConfigureParams {
            method: "native".into(),
            input_device: Some(options.microphone.clone()),
            output_device: Some(OUTPUT_ID.into()),
            input_channel: Some(1),
            output_channels,
            ..Default::default()
        }),
    )?;
    if let Some(file) = microphone_calibration {
        call(&engine, Request::MicCalibrationSet(MicCalibrationSetParams { path: file.display().to_string() }))?;
    }
    call(&engine, Request::ProjectSave(ProjectSaveParams::default()))?;
    progress("started", &format!("{} speakers, {subs} subwoofers", mains.len()));
    Ok(())
}

fn open(options: &Options) -> Result<Engine> {
    let engine = engine(options, outputs(options)?.len());
    call(&engine, Request::ProjectOpen(ProjectPathParams { path: project_file(options).display().to_string() }))?;
    Ok(engine)
}

/// Measures every speaker from position `position` (1 first).
pub fn measure(options: &Options, position: i64) -> Result<()> {
    let engine = open(options)?;
    progress("measuring", &format!("position {position}"));
    job(&engine, Request::MeasurePosition(MeasurePositionParams { position }))?;
    call(&engine, Request::ProjectSave(ProjectSaveParams::default()))?;
    progress("measured", &format!("position {position}"));
    Ok(())
}

/// Designs the correction and exports it into `out`, which must not exist.
pub fn finish(options: &Options, out: &Path) -> Result<()> {
    if out.exists() {
        return Err(format!("{} already exists", out.display()).into());
    }
    let engine = open(options)?;
    progress("designing", "");
    job(&engine, Request::Calibrate(CalibrateParams::default()))?;
    progress("exporting", &out.display().to_string());
    job(
        &engine,
        Request::Export(ExportParams {
            kind: ExportKind::CamillaDsp,
            folder: out.display().to_string(),
            mdat_scope: None,
            design_name: None,
            camilla_devices: None,
            extra_rates: Vec::new(),
        }),
    )?;
    call(&engine, Request::ProjectSave(ProjectSaveParams::default()))?;
    progress("done", &out.display().to_string());
    Ok(())
}
