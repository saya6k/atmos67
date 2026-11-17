//! Success criterion 1 of tasks/dsp.md: SECS 2R designs a correction for its
//! embedded 7.1.4 sample room, and the CamillaDSP export, as atmos67 runs it,
//! must reproduce the Convolution export's impulse response for every
//! input-to-output path. Ignored by default: the design takes minutes.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use secs::types::ipc::{
    CalibrateParams, ExportKind, ExportParams, JobEvent, ProjectNewParams, SampleLoadParams,
};
use secs::{Engine, Request, Response, Secs};

use super::camilla::{Config, Pipeline, wav_samples};

const BLOCK: usize = 1024;

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn run(engine: &Engine, request: Request) {
    let job = match engine.call(request).unwrap() {
        Response::SampleLoad(started) | Response::Calibrate(started) | Response::Export(started) => started.job,
        other => panic!("unexpected response {other:?}"),
    };
    match engine.wait_job(job, Duration::from_secs(3600)).unwrap() {
        JobEvent::Finished { .. } => {}
        other => panic!("job ended with {other:?}"),
    }
}

fn export(engine: &Engine, kind: ExportKind, folder: &Path) {
    run(
        engine,
        Request::Export(ExportParams {
            kind,
            folder: folder.display().to_string(),
            mdat_scope: None,
            design_name: None,
            camilla_devices: None,
            extra_rates: Vec::new(),
        }),
    );
}

#[test]
#[ignore = "designs a correction with SECS 2R, which takes minutes"]
fn camilla_export_matches_the_convolution_export() {
    // Kept under target/ so a failure can be examined and the next run can
    // skip the design.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/secs-parity");
    if !root.join("convolution").exists() {
        design(&root);
    }
    compare(&root);
}

fn design(root: &Path) {
    let _ = fs::remove_dir_all(root);
    fs::create_dir_all(root).unwrap();
    let engine = Engine::new();
    engine
        .call(Request::ProjectNew(ProjectNewParams {
            path: root.join("room.secs").display().to_string(),
            preset: "7.1.4".into(),
            sub_count: 1,
            sample: true,
            tutorial: false,
        }))
        .unwrap();
    run(&engine, Request::SampleLoad(SampleLoadParams { positions: None }));
    run(&engine, Request::Calibrate(CalibrateParams::default()));
    export(&engine, ExportKind::CamillaDsp, &root.join("camilla"));
    export(&engine, ExportKind::Convolution, &root.join("convolution"));
}

fn compare(root: &Path) {

    let mut camilla = Vec::new();
    files(&root.join("camilla"), &mut camilla);
    let yml = camilla.iter().find(|p| p.extension().is_some_and(|e| e == "yml")).expect("a .yml");
    let manifest = camilla.iter().find(|p| p.ends_with("secs-design.json")).expect("secs-design.json");
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
    let names = |key: &str| -> Vec<String> {
        manifest[key].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
    };
    let (inputs, outputs) = (names("inputs"), names("outputs"));

    let mut paths = HashMap::new();
    let mut convolution = Vec::new();
    files(&root.join("convolution"), &mut convolution);
    for file in convolution.iter().filter(|p| p.extension().is_some_and(|e| e == "wav")) {
        let stem = file.file_stem().unwrap().to_str().unwrap();
        let (input, output) = stem.split_once("_to_").expect("<input>_to_<output>.wav");
        paths.insert((input.to_string(), output.to_string()), wav_samples(&fs::read(file).unwrap()).unwrap());
    }
    assert!(!paths.is_empty(), "no convolution files in {convolution:?}");
    let length = paths.values().map(Vec::len).max().unwrap() + BLOCK;
    let peak = paths.values().flatten().fold(0f64, |m, s| m.max(s.abs()));

    let config = Config::load(yml).unwrap();
    assert_eq!(config.inputs(), inputs.len(), "inputs {inputs:?}");
    let mut worst = (0.0, String::new());
    for (index, input) in inputs.iter().enumerate() {
        let mut pipeline = Pipeline::new(&config, BLOCK).unwrap();
        assert_eq!(pipeline.outputs(), outputs.len(), "outputs {outputs:?}");
        let mut got = vec![Vec::new(); outputs.len()];
        for start in (0..length).step_by(BLOCK) {
            let mut channels = vec![vec![0.0; BLOCK]; inputs.len()];
            if start == 0 {
                channels[index][0] = 1.0;
            }
            pipeline.process(&mut channels);
            for (all, block) in got.iter_mut().zip(channels) {
                all.extend(block);
            }
        }
        for (output, response) in outputs.iter().zip(&got) {
            let expected = paths.get(&(input.clone(), output.clone()));
            let argmax = |v: &[f64]| v.iter().enumerate().fold((0, 0f64), |m, (i, s)| if s.abs() > m.1.abs() { (i, *s) } else { m });
            let (gi, gv) = argmax(response);
            let (wi, wv) = expected.map_or((0, 0.0), |e| argmax(e));
            if gv.abs() > peak * 1e-3 || wv.abs() > peak * 1e-3 {
                eprintln!("{input:>4} -> {output:<4} got peak {gv:+.4} at {gi:6}  want {wv:+.4} at {wi:6}  len {}", expected.map_or(0, Vec::len));
            }
            for (n, sample) in response.iter().enumerate() {
                let want = expected.and_then(|e| e.get(n)).copied().unwrap_or(0.0);
                let error = (sample - want).abs() / peak;
                if error > worst.0 {
                    worst = (error, format!("{input} -> {output} at sample {n}: {sample} vs {want}"));
                }
            }
        }
    }
    eprintln!("worst relative error {:.2e} ({})", worst.0, worst.1);
    assert!(worst.0 < 1e-3, "{}", worst.1);
}
