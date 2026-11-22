//! Find and stop the bundled server, including the workers it spawns.
use std::path::{Path, PathBuf};
use std::process::Command;

/// The server ships as a PyInstaller folder in the app's resources, so it
/// starts in place instead of unpacking itself to a temp dir on every launch.
pub fn bundled_executable(resource_dir: &Path) -> Result<PathBuf, String> {
    let executable = resource_dir.join("kass-server").join("kass-server");
    if executable.is_file() {
        Ok(executable)
    } else {
        Err(format!(
            "Bundled server not found at {}",
            executable.display()
        ))
    }
}

fn descendants(root: u32, processes: &[(u32, u32)]) -> Vec<u32> {
    let mut result = vec![root];
    let mut index = 0;
    while index < result.len() {
        let parent = result[index];
        for &(pid, ppid) in processes {
            if ppid == parent && !result.contains(&pid) {
                result.push(pid);
            }
        }
        index += 1;
    }
    result.reverse();
    result
}

pub fn stop(pid: u32) -> Result<(), String> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid="])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("Could not inspect the server process tree".into());
    }
    let processes: Vec<(u32, u32)> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect();
    let pids = descendants(pid, &processes);
    for signal in ["-TERM", "-KILL"] {
        for child in &pids {
            let _ = Command::new("kill")
                .args([signal, &child.to_string()])
                .output();
        }
        if signal == "-TERM" {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_server_inside_its_bundled_folder() {
        let resources = std::env::temp_dir().join(format!("vb-resources-{}", std::process::id()));
        let folder = resources.join("kass-server");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("kass-server"), "").unwrap();
        let found = bundled_executable(&resources);
        std::fs::remove_dir_all(&resources).unwrap();
        assert_eq!(found, Ok(folder.join("kass-server")));
    }

    #[test]
    fn reports_a_missing_bundled_server() {
        let resources = std::env::temp_dir().join("vb-resources-missing");
        assert!(bundled_executable(&resources).is_err());
    }

    #[test]
    fn includes_nested_workers_but_not_unrelated_processes() {
        assert_eq!(
            descendants(10, &[(30, 20), (20, 10), (11, 1), (10, 1)]),
            vec![30, 20, 10]
        );
    }

    #[test]
    fn stops_worker_outside_a_dedicated_process_group() {
        use std::io::{BufRead, BufReader};
        use std::process::Stdio;
        let mut parent = Command::new("sh")
            .args(["-c", "sleep 120 & echo $!; wait"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(parent.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let worker: u32 = line.trim().parse().unwrap();
        stop(parent.id()).unwrap();
        parent.wait().unwrap();
        let output = Command::new("ps")
            .args(["-p", &worker.to_string(), "-o", "stat="])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&output.stdout);
        assert!(
            state.trim().is_empty() || state.trim().starts_with('Z'),
            "worker still alive: {state}"
        );
    }
}
