use clap::Parser;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};

// ... Args struct and get_probe_value remain the same ...

#[derive(Parser, Debug)]
#[command(author, version, about = "Unified Streamer: Stream local files or URLs to VLC")]
#[clap(disable_version_flag = true, disable_help_flag = true)]
struct Args {
    #[arg(short = 'l')]
    local_file: Option<String>,
    #[arg(short = 'u')]
    url: Option<String>,
    #[arg(short = 'p')]
    playlist: Option<String>,
    #[arg(short = 'x', default_value_t = 8080)]
    port: u16,
    #[arg(short = 'h', long = "help", action = clap::ArgAction::Help)]
    help: Option<bool>,
    #[arg(short = 'v', long = "version", action = clap::ArgAction::Version)]
    version: Option<bool>,
}

fn get_probe_value(file: &str, entry: &str) -> String {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", entry, "-of", "default=noprint_wrappers=1:nokey=1", file])
        .output().expect("ffprobe failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

// Updated to return (Title, Vec<StreamURLs>)
fn get_yt_data(input: &str) -> (String, Vec<String>) {
    let output = Command::new("yt-dlp")
        .args([
            "--no-warnings",
            "-f", "bestvideo[vcodec^=avc1]+bestaudio[acodec^=mp4a]/best[vcodec^=avc1]",
            "--print", "title", // Print title on the first line
            "-g",               // Print URLs on subsequent lines
            "--no-playlist",
            input,
        ])
        .output()
        .expect("Failed to run yt-dlp");

    let out_str = String::from_utf8_lossy(&output.stdout);
    let mut lines = out_str.lines();
    
    // The first line is the title, the rest are URLs
    let title = lines.next().unwrap_or(input).to_string();
    let urls: Vec<String> = lines.map(|s| s.to_string()).collect();

    (title, urls)
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

        if v_codec == "h264" {
            ffmpeg_args.extend(vec!["-c:v".to_string(), "copy".to_string()]);
        } else {
            ffmpeg_args.extend(vec!["-c:v".to_string(), "libx264".to_string(), "-preset".to_string(), "ultrafast".to_string(), "-crf".to_string(), "23".to_string()]);
        }
        
        if a_codec == "aac" {
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

    ffmpeg_args.extend(vec!["-tune".to_string(), "zerolatency".to_string(), "-f".to_string(), "mpegts".to_string(), "-".to_string()]);

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
                let _ = child.kill();
                let _ = child.wait();
                #[cfg(unix)]
                { let _ = Command::new("stty").arg("sane").status(); }
                return;
            }
        }
    }
    let _ = child.wait();
}

fn main() {
    let args = Args::parse();
    let addr = format!("0.0.0.0:{}", args.port);
    let listener = TcpListener::bind(&addr).expect("Could not bind to port");
    println!("+ listening");

    let (mut client, _) = listener.accept().expect("Failed to accept connection");
    let mut initial_buffer = [0; 1024];
    let _ = client.read(&mut initial_buffer);

    let response = "HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nConnection: keep-alive\r\n\r\n";
    let _ = client.write_all(response.as_bytes());

    if let Some(file) = args.local_file {
        println!("+ streaming: {}", file);
        run_ffmpeg(vec![file], true, &client);
    } else if let Some(url) = args.url {
        println!("+ fetching: {}", url);
        let (title, urls) = get_yt_data(&url); // Use the new function
        println!("+ streaming: {}", title);     // Display title
        run_ffmpeg(urls, false, &client);
    } else if let Some(playlist_url) = args.playlist {
        println!("+ loading playlist...");
        let output = Command::new("yt-dlp")
            .args(["--flat-playlist", "--print", "id", &playlist_url])
            .output()
            .expect("Failed to get playlist IDs");

        for id in String::from_utf8_lossy(&output.stdout).lines() {
            let full_url = format!("https://www.youtube.com/watch?v={}", id);
            println!("+ fetching: {}", full_url);
            let (title, urls) = get_yt_data(&full_url); // Get title for each item
            println!("+ streaming: {}", title);         // Display title
            run_ffmpeg(urls, false, &client);
        }
    }

    println!("\n+ done");
    let _ = client.shutdown(std::net::Shutdown::Both);
}
