use std::fs::{self, File};
use std::io::Write;
use std::{cmp::min, path::Path};

use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Client;

/// Download ECC from S3
///
/// Adapted from:
/// https://gist.github.com/giuliano-oliveira/4d11d6b3bb003dba3a1b53f43d81b30d
pub async fn get_ecc_from_s3(client: &Client, ecc_name: &str) -> Result<(), String> {
    let url = format!("https://eccs.eu-central-1.linodeobjects.com/{ecc_name}.json");

    // Reqwest setup
    let res = client
        .get(&url)
        .send()
        .await
        .or(Err(format!("Failed to GET from '{}'", &url)))?;
    let total_size = res
        .content_length()
        .ok_or(format!("Failed to get content length from '{}'", &url))?;

    // Indicatif setup
    let pb = ProgressBar::new(total_size);
    pb.set_style(ProgressStyle::default_bar()
        .template("{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")
        .progress_chars("#>-"));
    pb.set_message(&format!("Downloading {}", url));

    // download chunks
    let path = format!("data/{ecc_name}.json");
    let mut file = File::create(&path).or(Err(format!("Failed to create file '{path}'")))?;
    let mut downloaded: u64 = 0;
    let mut stream = res.bytes_stream();

    while let Some(item) = stream.next().await {
        let chunk = item.or(Err(format!("Error while downloading file")))?;
        file.write_all(&chunk)
            .or(Err(format!("Error while writing to file")))?;
        let new = min(downloaded + (chunk.len() as u64), total_size);
        downloaded = new;
        pb.set_position(new);
    }

    pb.finish_with_message(&format!("Downloaded {} to {}", url, path));
    return Ok(());
}

/// Ensure that the ECC file exists by downloading it otherwise
#[tokio::main]
pub async fn ensure_exists(ecc_name: &str) -> Result<(), String> {
    let path = format!("data/{ecc_name}.json");
    if !Path::new(&path).exists() {
        fs::create_dir_all("data").or(Err(format!("Failed to create data directory")))?;
        let client = Client::new();
        get_ecc_from_s3(&client, ecc_name).await?;
    }
    Ok(())
}
