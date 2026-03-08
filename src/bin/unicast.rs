use clap::Parser;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};

#[derive(Parser, Debug)]
#[command(author, version, about = "Unified Streamer: Stream local files or URLs to VLC")]
#[clap(disable_version_flag = true, disable_help_flag = true)]
struct Args {
    /// Local file to stream
    #[arg(short = 'l')]
    local_file: Option<String>,

    /// Single URL to stream
    #[arg(short = 'u')]
    url: Option<String>,

    /// Playlist URL to stream
    #[arg(short = 'p')]
    playlist: Option<String>,

    /// Port to listen on
    #[arg(short = 'x', default_value_t = 8080)]
    port: u16,

    /// Print help
    #[arg(short = 'h', long = "help", action = clap::ArgAction::Help)]
    help: Option<bool>,

    /// Print version
    #[arg(short = 'v', long = "version", action = clap::ArgAction::Version)]
    version: Option<bool>,
}

fn get_probe_value(file: &str, entry: &str) -> String {
    let output = Command::new("ffprobe")
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", entry,
            "-of", "default=noprint_wrappers=1:nokey=1",
            file,
        ])
        .output()
        .expect("ffprobe failed");
    
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn run_ffmpeg(urls: Vec<String>, is_local: bool, mut client: &TcpStream) {
    let mut ffmpeg_args = vec![
        "-hide_banner".to_string(),
        "-stats".to_string(),
        "-v".to_string(), "panic".to_string(),
        "-re".to_string(),
    ];

    for url in &urls {
        ffmpeg_args.push("-i".to_string());
        ffmpeg_args.push(url.clone());
    }

    if is_local {
        let file = &urls[0];
        let v_codec = get_probe_value(file, "stream=codec_name");
        
        let a_codec_output = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "a:0", "-show_entries", "stream=codec_name", "-of", "default=noprint_wrappers=1:nokey=1", file])
            .output().expect("ffprobe audio codec failed");
        let a_codec = String::from_utf8_lossy(&a_codec_output.stdout).trim().to_string();

        let a_chan_output = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "a:0", "-show_entries", "stream=channels", "-of", "default=noprint_wrappers=1:nokey=1", file])
            .output().expect("ffprobe audio channels failed");
        let a_channels: i32 = String::from_utf8_lossy(&a_chan_output.stdout).trim().parse().unwrap_or(0);

        if v_codec == "h264" {
            ffmpeg_args.extend(vec!["-c:v".to_string(), "copy".to_string()]);
        } else {
            ffmpeg_args.extend(vec!["-c:v".to_string(), "libx264".to_string(), "-preset".to_string(), "ultrafast".to_string(), "-crf".to_string(), "23".to_string()]);
        }

        if a_codec == "aac" && a_channels <= 2 {
            ffmpeg_args.extend(vec!["-c:a".to_string(), "copy".to_string()]);
        } else {
            ffmpeg_args.extend(vec!["-c:a".to_string(), "aac".to_string(), "-b:a".to_string(), "192k".to_string(), "-ac".to_string(), "2".to_string()]);
        }
    } else {
        ffmpeg_args.extend(vec!["-c:v".to_string(), "copy".to_string(), "-c:a".to_string(), "copy".to_string()]);
    }

    if urls.len() == 2 {
        ffmpeg_args.extend(vec!["-map".to_string(), "0:0".to_string(), "-map".to_string(), "1:0".to_string()]);
    }

    ffmpeg_args.extend(vec![
        "-tune".to_string(), "zerolatency".to_string(),
        "-f".to_string(), "mpegts".to_string(),
        "-".to_string(), 
    ]);

    let mut child = Command::new("ffmpeg")
        .args(&ffmpeg_args)
        .stdin(Stdio::inherit()) 
        .stdout(Stdio::piped())
        .spawn()
        .expect("ffmpeg failed to start");

    if let Some(mut stdout) = child.stdout.take() {
        let mut buffer = [0; 16384];
        while let Ok(n) = stdout.read(&mut buffer) {
            if n == 0 { break; }
            if client.write_all(&buffer[..n]).is_err() {
                // Network failed. Use nix crate or libc to send SIGINT for clean exit.
                // As a fallback for standard Command, we use kill, but we must
                // immediately follow up with stty sane to guarantee terminal recovery.
                let _ = child.kill();
                let _ = child.wait();

                // Only run stty sane on Unix platforms (Linux, macOS, FreeBSD, NixOS)
                #[cfg(unix)]
                {
                    let _ = Command::new("stty").arg("sane").status();
                }

                println!("\n+ disconnected: terminal restored");
                return;
            }
        }
    }
    let _ = child.wait();
}

fn get_yt_urls(input: &str) -> Vec<String> {
    let output = Command::new("yt-dlp")
        .args([
            "--no-warnings",
            "-f", "bestvideo[vcodec^=avc1]+bestaudio[acodec^=mp4a]/best[vcodec^=avc1]",
            "-g",
            "--no-playlist",
            input,
        ])
        .output()
        .expect("Failed to run yt-dlp");

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect()
}

fn main() {
    let args = Args::parse();
    let addr = format!("0.0.0.0:{}", args.port);
    
    let listener = TcpListener::bind(&addr).expect("Could not bind to port");
    println!("+ stream ready");

    let (mut client, _) = listener.accept().expect("Failed to accept connection");

    let mut initial_buffer = [0; 1024];
    let _ = client.read(&mut initial_buffer);

    let response = "HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nConnection: keep-alive\r\n\r\n";
    if let Err(e) = client.write_all(response.as_bytes()) {
        eprintln!("! Failed to send HTTP header: {}", e);
        return;
    }

    if let Some(file) = args.local_file {
        run_ffmpeg(vec![file], true, &client);
    } else if let Some(url) = args.url {
        let urls = get_yt_urls(&url);
        run_ffmpeg(urls, false, &client);
    } else if let Some(playlist_url) = args.playlist {
        println!("+ Loading playlist...");
        let output = Command::new("yt-dlp")
            .args(["--flat-playlist", "--print", "id", &playlist_url])
            .output()
            .expect("Failed to get playlist IDs");

        for id in String::from_utf8_lossy(&output.stdout).lines() {
            let full_url = format!("https://www.youtube.com/watch?v={}", id);
            println!("+ Streaming: {}", id);
            let urls = get_yt_urls(&full_url);
            run_ffmpeg(urls, false, &client);
        }
    }

    println!("\n+ Stream finished. Closing connection...");
    let _ = client.shutdown(std::net::Shutdown::Both);
}
