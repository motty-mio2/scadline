use std::{ffi::OsString, path::PathBuf};

use crate::infrastructure::ModelBackend;

pub(crate) struct CliOptions {
    pub(crate) source_path: Option<PathBuf>,
    pub(crate) backend: Option<ModelBackend>,
}

pub(crate) enum CliAction {
    Run(CliOptions),
    Help,
}

pub(crate) const HELP: &str = "Scadline - OpenSCAD viewer\n\nUsage: scadline [OPTIONS] [FILE.scad]\n\nOptions:\n      --backend <BACKEND>  Model backend: cgal, manifold, or openrscad\n  -h, --help               Print help\n";

pub(crate) fn parse_args<I>(arguments: I) -> Result<CliAction, String>
where
    I: IntoIterator<Item = OsString>,
{
    let mut arguments = arguments.into_iter();
    let mut source_path = None;
    let mut backend = None;

    while let Some(argument) = arguments.next() {
        if argument == "-h" || argument == "--help" {
            return Ok(CliAction::Help);
        }
        if argument == "--backend" {
            let value = arguments
                .next()
                .ok_or_else(|| "--backend には値が必要です".to_owned())?;
            let value = value
                .to_str()
                .ok_or_else(|| "--backend の値はUTF-8で指定してください".to_owned())?;
            backend = Some(value.parse()?);
            continue;
        }
        if let Some(value) = argument
            .to_str()
            .and_then(|value| value.strip_prefix("--backend="))
        {
            backend = Some(value.parse()?);
            continue;
        }
        if argument.to_string_lossy().starts_with('-') {
            return Err(format!(
                "不明なオプションです: {}",
                argument.to_string_lossy()
            ));
        }
        if source_path.replace(PathBuf::from(argument)).is_some() {
            return Err("指定できるファイルは1つだけです".to_owned());
        }
    }

    Ok(CliAction::Run(CliOptions {
        source_path,
        backend,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_backend_and_source_in_any_order() {
        let CliAction::Run(options) =
            parse_args(["examples/demo.scad".into(), "--backend=manifold".into()])
                .expect("arguments should parse")
        else {
            panic!("expected run action");
        };
        assert_eq!(
            options.source_path,
            Some(PathBuf::from("examples/demo.scad"))
        );
        assert_eq!(options.backend, Some(ModelBackend::Manifold));
    }

    #[test]
    fn rejects_unknown_backend() {
        assert!(parse_args(["--backend".into(), "fast".into()]).is_err());
    }

    #[test]
    fn parses_openrscad_backend() {
        let CliAction::Run(options) =
            parse_args(["--backend=openrscad".into()]).expect("OpenRSCAD backend should parse")
        else {
            panic!("expected run action");
        };
        assert_eq!(options.backend, Some(ModelBackend::Openrscad));
    }
}
