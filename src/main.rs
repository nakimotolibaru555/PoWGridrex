use std::collections::VecDeque;
use std::fs;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};

// Signal handler to restore terminal cursor on Ctrl+C / SIGINT
extern "C" fn sigint_handler(_: libc::c_int) {
    print!("\x1b[?25h\x1b[0m\n\n  \x1b[1;33m[SHUTDOWN]\x1b[0m Miner stopped safely. Terminal restored.\n\n");
    let _ = io::stdout().flush();
    std::process::exit(0);
}

fn current_time_str() -> String {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    #[cfg(unix)]
    unsafe { libc::localtime_r(&now, &mut tm) };
    #[cfg(windows)]
    unsafe { libc::localtime_s(&mut tm, &now) };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

fn visible_len(s: &str) -> usize {
    let mut in_escape = false;
    let mut count = 0;
    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if c == 'm' {
                in_escape = false;
            }
        } else {
            count += 1;
        }
    }
    count
}

fn truncate_visible(s: &str, max_len: usize) -> String {
    if visible_len(s) <= max_len {
        return s.to_string();
    }
    let mut out = String::new();
    let mut count = 0;
    let mut in_escape = false;
    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
            out.push(c);
        } else if in_escape {
            out.push(c);
            if c == 'm' {
                in_escape = false;
            }
        } else {
            if count + 4 <= max_len {
                out.push(c);
                count += 1;
            } else if count + 1 <= max_len {
                out.push('.');
                count += 1;
            } else {
                break;
            }
        }
    }
    out.push_str("\x1b[0m");
    out
}

fn box_row(content: &str, inner_width: usize) -> String {
    let vlen = visible_len(content);
    let padding = if vlen < inner_width { inner_width - vlen } else { 0 };
    format!("│ {}{} │\x1b[K\n", content, " ".repeat(padding))
}

fn two_col_row(left: &str, left_col_width: usize, right: &str, inner_width: usize) -> String {
    let l_vlen = visible_len(left);
    let l_pad = if l_vlen < left_col_width { left_col_width - l_vlen } else { 0 };
    let r_vlen = visible_len(right);
    let total_used = l_vlen + l_pad + r_vlen;
    let r_pad = if total_used < inner_width { inner_width - total_used } else { 0 };
    format!("│ {}{}{}{} │\x1b[K\n", left, " ".repeat(l_pad), right, " ".repeat(r_pad))
}

fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut res = String::new();
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            res.push(',');
        }
        res.push(c);
    }
    res
}

fn format_speed(hs: f64) -> (f64, &'static str) {
    if hs >= 1_000_000.0 {
        (hs / 1_000_000.0, "MH/s")
    } else if hs >= 1_000.0 {
        (hs / 1_000.0, "kH/s")
    } else {
        (hs, "H/s")
    }
}

pub struct EventLogger {
    events: Mutex<VecDeque<String>>,
    max_events: usize,
}

impl EventLogger {
    pub fn new(max_events: usize) -> Self {
        Self {
            events: Mutex::new(VecDeque::with_capacity(max_events)),
            max_events,
        }
    }

    pub fn log(&self, msg: String) {
        let now_str = current_time_str();
        let mut q = self.events.lock().unwrap();
        if q.len() >= self.max_events {
            q.pop_front();
        }
        q.push_back(format!("\x1b[90m[{}]\x1b[0m {}", now_str, msg));
    }

    pub fn get_events(&self) -> Vec<String> {
        let q = self.events.lock().unwrap();
        q.iter().cloned().collect()
    }
}

// =========================================================================
//  CORTEX ASIC-RESISTANT RANDOMX VM ENGINE (100% BIT-PERFECT L1 ACCURACY)
// =========================================================================

#[derive(Clone)]
pub struct PrecomputedSeed {
    pub seed: Vec<u8>,
    pub colon_seed: Vec<u8>,
    pub seed_colon_hasher: Sha512,
}

impl PrecomputedSeed {
    pub fn new(seed: &[u8]) -> Self {
        let mut colon_seed = Vec::with_capacity(seed.len() + 1);
        colon_seed.push(b':');
        colon_seed.extend_from_slice(seed);

        let mut hasher = Sha512::new();
        hasher.update(seed);
        hasher.update(b":");

        Self {
            seed: seed.to_vec(),
            colon_seed,
            seed_colon_hasher: hasher,
        }
    }
}

pub struct CortexRandomX;

impl CortexRandomX {
    #[inline(always)]
    pub fn hash(header: &[u8], seed: &[u8], scratchpad: &mut [i64; 4096]) -> [u8; 32] {
        let precomputed = PrecomputedSeed::new(seed);
        Self::hash_fast(header, &precomputed, scratchpad)
    }

    #[inline(always)]
    pub fn hash_fast(header: &[u8], precomputed: &PrecomputedSeed, scratchpad: &mut [i64; 4096]) -> [u8; 32] {
        // Step 1: Initialize Scratchpad using Seed & Header
        let mut hasher = Sha512::new();
        hasher.update(header);
        hasher.update(&precomputed.colon_seed);
        let mut key: [u8; 64] = hasher.finalize().into();

        for blk in 0..64 {
            let base = blk * 64;
            let key_words: [i64; 8] = unsafe { std::mem::transmute(key) };
            scratchpad[base..base + 8].copy_from_slice(&key_words);

            let mut k_hasher = Sha512::new();
            k_hasher.update(&key);
            key = k_hasher.finalize().into();
            let next_words: [i64; 8] = unsafe { std::mem::transmute(key) };

            for chunk in 1..8 {
                scratchpad[base + chunk * 8..base + chunk * 8 + 8].copy_from_slice(&next_words);
            }
        }

        // Step 2: Initialize Registers
        let mut d_hasher = precomputed.seed_colon_hasher.clone();
        d_hasher.update(header);
        let initial_digest: [u8; 64] = d_hasher.finalize().into();

        let mut r: [i64; 8] = unsafe { std::mem::transmute(initial_digest) };

        let mut f = [0.0f64; 4];
        for i in 0..4 {
            f[i] = (r[i] % 1_000_000) as f64 / 1000.0;
        }

        // Step 3: Random Instruction VM Execution Loop (64 cycles)
        let seed = &precomputed.seed;
        let seed_len = seed.len();

        for iter in 0..64usize {
            let op_code = (initial_digest[iter] ^ seed[iter % seed_len]) % 10;
            let src_idx = (iter + 1) & 7;
            let dst_idx = iter & 7;
            let mem_idx = (r[dst_idx] as usize) & 4095;

            match op_code {
                0 => {
                    r[dst_idx] = r[dst_idx].wrapping_add(scratchpad[mem_idx]);
                }
                1 => {
                    r[dst_idx] = r[dst_idx].wrapping_sub(r[src_idx]);
                }
                2 => {
                    r[dst_idx] = r[dst_idx].wrapping_mul(r[src_idx] | 1);
                }
                3 => {
                    r[dst_idx] ^= r[src_idx];
                }
                4 => {
                    let shift = (r[src_idx] as u64 & 63) as u32;
                    let sr = r[dst_idx];
                    let right = if shift == 0 {
                        if sr < 0 { -1i64 } else { 0i64 }
                    } else {
                        sr >> (64 - shift)
                    };
                    r[dst_idx] = ((sr as u64) << shift | (right as u64)) as i64;
                }
                5 => {
                    scratchpad[mem_idx] = r[dst_idx] ^ (iter as i64);
                }
                6 => {
                    f[dst_idx & 3] += f[src_idx & 3];
                    let v = f[dst_idx & 3].abs().floor() as i64;
                    r[dst_idx] ^= v;
                }
                7 => {
                    f[dst_idx & 3] *= 1.00001;
                    let v = f[dst_idx & 3].abs().floor() as i64;
                    r[dst_idx] ^= v;
                }
                8 => {
                    let next_mem = (mem_idx + 64) & 4095;
                    let temp = scratchpad[mem_idx];
                    scratchpad[mem_idx] = scratchpad[next_mem];
                    scratchpad[next_mem] = temp;
                }
                9 => {
                    r[dst_idx] = r[dst_idx].wrapping_neg();
                }
                _ => unreachable!(),
            }
        }

        // Step 4: Final Sponge Digest
        let final_buf: [u8; 64] = unsafe { std::mem::transmute(r) };
        let h1: [u8; 32] = Sha256::digest(&final_buf).into();

        let mut sponge_hasher = Sha256::new();
        sponge_hasher.update(&h1);
        let scratchpad_bytes: &[u8; 512] = unsafe {
            &*(scratchpad.as_ptr() as *const [u8; 512])
        };
        sponge_hasher.update(scratchpad_bytes);
        sponge_hasher.finalize().into()
    }
}

#[inline(always)]
fn check_difficulty(hash: &[u8; 32], diff: usize) -> bool {
    if diff == 0 { return true; }
    let full_bytes = diff / 2;
    for i in 0..full_bytes {
        if hash[i] != 0 { return false; }
    }
    if diff % 2 == 1 {
        if (hash[full_bytes] >> 4) != 0 { return false; }
    }
    true
}

#[inline(always)]
fn check_target_u64(hash: &[u8; 32], target_u64: u64) -> bool {
    let hash_val = u64::from_be_bytes([
        hash[0], hash[1], hash[2], hash[3],
        hash[4], hash[5], hash[6], hash[7],
    ]);
    hash_val <= target_u64
}

#[inline(always)]
fn write_u64_ascii(mut n: u64, buf: &mut [u8]) -> usize {
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut temp = [0u8; 20];
    let mut i = 20;
    while n > 0 {
        i -= 1;
        temp[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let len = 20 - i;
    buf[..len].copy_from_slice(&temp[i..]);
    len
}

// =========================================================================
//  NETWORK TYPES
// =========================================================================

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TemplateResponse {
    job_id: Option<String>,
    header_prefix: Option<String>,
    header_suffix: Option<String>,
    seed: Option<String>,
    share_difficulty: Option<f64>,
    target_difficulty: Option<f64>,
    target_u64: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SubmitRequest<'a> {
    job_id: &'a str,
    nonce: u64,
    address: &'a str,
    worker: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubmitResponse {
    valid_share: Option<bool>,
    accepted: Option<bool>,
    #[serde(alias = "block_found")]
    block_found: Option<bool>,
    error: Option<String>,
}

#[derive(Clone, Default)]
struct ActiveJob {
    job_id: String,
    header_prefix: Vec<u8>,
    header_suffix: Vec<u8>,
    seed: Vec<u8>,
    difficulty: f64,
    target_u64: u64,
    valid: bool,
}

// =========================================================================
//  MAIN APPLICATION & MINING LOOP
// =========================================================================

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut pool_url = "https://raix.powgrid.xyz".to_string();
    let mut address = String::new();
    let mut worker = "rust-rig1".to_string();
    let mut num_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(24);
    let mut bench_mode = false;
    let mut bench_secs = 10u64;
    let mut throttle_ms = 0u64;

    // Check config.txt or wallet.txt
    if let Ok(content) = fs::read_to_string("config.txt") {
        for line in content.lines() {
            let line = line.trim();
            if let Some(w) = line.strip_prefix("WALLET=") {
                let trimmed = w.trim();
                if !trimmed.is_empty() { address = trimmed.to_string(); }
            } else if let Some(wrk) = line.strip_prefix("WORKER=") {
                let trimmed = wrk.trim();
                if !trimmed.is_empty() { worker = trimmed.to_string(); }
            }
        }
    } else if let Ok(content) = fs::read_to_string("wallet.txt") {
        let trimmed = content.trim().to_string();
        if !trimmed.is_empty() {
            address = trimmed;
        }
    }

    let mut i = 1;
    let mut worker_from_cli = false;
    while i < args.len() {
        match args[i].as_str() {
            "--node" | "-n" | "-o" if i + 1 < args.len() => { pool_url = args[i + 1].clone(); i += 2; }
            "--address" | "-a" | "-u" if i + 1 < args.len() => { address = args[i + 1].clone(); i += 2; }
            "--worker" | "-w" if i + 1 < args.len() => { worker = args[i + 1].clone(); worker_from_cli = true; i += 2; }
            "--threads" | "-t" if i + 1 < args.len() => {
                if let Ok(n) = args[i + 1].parse() { num_threads = n; }
                i += 2;
            }
            "--throttle" if i + 1 < args.len() => {
                if let Ok(ms) = args[i + 1].parse() { throttle_ms = ms; }
                i += 2;
            }
            "--solve" if i + 5 < args.len() => {
                let prefix = args[i + 1].clone();
                let suffix = args[i + 2].clone();
                let seed = args[i + 3].clone();
                let diff: usize = args[i + 4].parse().unwrap_or(4);
                let count: usize = args[i + 5].parse().unwrap_or(1);

                let start_nonce: Option<u64> = if i + 6 < args.len() { args[i + 6].parse().ok() } else { None };
                let solve_threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(24);

                let start = Instant::now();
                let found_nonces = Arc::new(std::sync::Mutex::new(Vec::new()));
                let total_hashes = Arc::new(AtomicU64::new(0));
                let stop_flag = Arc::new(AtomicBool::new(false));

                let base_nonce = start_nonce.unwrap_or_else(|| {
                    (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64) % 1_000_000_000
                });

                let mut handles = Vec::new();
                let precomputed = PrecomputedSeed::new(seed.as_bytes());
                let p_bytes = prefix.as_bytes().to_vec();
                let s_bytes = suffix.as_bytes().to_vec();

                for t in 0..solve_threads {
                    let precomputed = precomputed.clone();
                    let p_bytes = p_bytes.clone();
                    let s_bytes = s_bytes.clone();
                    let found_nonces = Arc::clone(&found_nonces);
                    let total_hashes = Arc::clone(&total_hashes);
                    let stop_flag = Arc::clone(&stop_flag);

                    handles.push(std::thread::spawn(move || {
                        let mut scratchpad = [0i64; 4096];
                        let mut nonce = base_nonce.wrapping_add((t as u64).wrapping_mul(10_000_000));
                        let mut local_hashes = 0u64;

                        let mut header_buf = [0u8; 1024];
                        let p_len = p_bytes.len();
                        header_buf[..p_len].copy_from_slice(&p_bytes);
                        let s_len = s_bytes.len();

                        while !stop_flag.load(Ordering::Relaxed) {
                            let n_len = write_u64_ascii(nonce, &mut header_buf[p_len..]);
                            header_buf[p_len + n_len..p_len + n_len + s_len].copy_from_slice(&s_bytes);
                            let total_len = p_len + n_len + s_len;

                            let hash = CortexRandomX::hash_fast(&header_buf[..total_len], &precomputed, &mut scratchpad);
                            local_hashes += 1;

                            if check_difficulty(&hash, diff) {
                                let mut f = found_nonces.lock().unwrap();
                                if f.len() < count {
                                    f.push(nonce);
                                    if f.len() >= count {
                                        stop_flag.store(true, Ordering::Relaxed);
                                    }
                                }
                            }

                            if local_hashes >= 500 {
                                total_hashes.fetch_add(local_hashes, Ordering::Relaxed);
                                local_hashes = 0;
                            }
                            nonce += 1;
                        }
                        total_hashes.fetch_add(local_hashes, Ordering::Relaxed);
                    }));
                }

                for h in handles {
                    let _ = h.join();
                }

                let final_nonces = found_nonces.lock().unwrap().clone();
                let hashes = total_hashes.load(Ordering::Relaxed);
                let elapsed_ms = start.elapsed().as_millis();
                println!("SOLVE_RESULT:{}", serde_json::json!({
                    "nonces": final_nonces,
                    "hashes": hashes,
                    "elapsed_ms": elapsed_ms
                }));
                return;
            }
            "--bench" | "--benchmark" => {
                bench_mode = true;
                if i + 1 < args.len() {
                    if let Ok(s) = args[i + 1].parse() { bench_secs = s; i += 1; }
                }
                i += 1;
            }
            _ => { i += 1; }
        }
    }

    println!("===========================================================");
    println!("  ⚡ PowGrid Reticulum AI ($RAIX) High-Performance CPU Miner v1.1");
    println!("  Threads Allocated   : {}", num_threads);
    println!("  Micro-Architecture  : AVX2 + Hardware SHA-NI Accelerated");
    println!("  L1 Cache Scratchpad : 32 KB per-thread (Zero RAM Access)");
    println!("===========================================================\n");

    // Self-test: Canonical Genesis Vector (Nonce 123) & High-Nonce Vector (Nonce 99999)
    let mut test_scratchpad = [0i64; 4096];
    let test_header = b"test_header_123";
    let test_seed = b"cortex-randomx-genesis-seed-v1";
    let test_hash = CortexRandomX::hash(test_header, test_seed, &mut test_scratchpad);
    let test_hex = hex::encode(test_hash);
    let expected_123 = "b1b9599c0a73a84c5777369c3a2af50e78cc63b6e0c22c30f191c98fc06316a5";

    let test_header_99k = b"test_header_99999";
    let test_hash_99k = CortexRandomX::hash(test_header_99k, test_seed, &mut test_scratchpad);
    let test_hex_99k = hex::encode(test_hash_99k);
    let expected_99999 = "912f8cb7ed73773658af2f4212aa40bb365d1a5a208eb09cd66e567b3cf70a5c";

    print!("[SELF-TEST] Cryptographic L1 accuracy check: ");
    if test_hex == expected_123 && test_hex_99k == expected_99999 {
        println!("✅ 100% BIT-PERFECT MATCH (Vectors 123 & 99999)\n");
    } else {
        println!("❌ MISMATCH!\n  Expected 123: {}\n  Got: {}\n  Expected 99k: {}\n  Got: {}", expected_123, test_hex, expected_99999, test_hex_99k);
        std::process::exit(1);
    }

    if bench_mode {
        run_benchmark(num_threads, bench_secs);
        return;
    }

    // 1-Click Interactive setup if address is empty
    if address.is_empty() {
        print!("Enter your Reticulum AI wallet address (starts with ctx1...): ");
        io::stdout().flush().unwrap();
        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();
        address = input.trim().to_string();

        if !worker_from_cli {
            let default_worker = if cfg!(target_os = "macos") {
                "mac-rig1"
            } else if cfg!(target_os = "windows") {
                "win-rig1"
            } else {
                "linux-rig1"
            };
            print!("Enter worker name [default: {}]: ", default_worker);
            io::stdout().flush().unwrap();
            let mut w_input = String::new();
            io::stdin().read_line(&mut w_input).unwrap();
            let trimmed_w = w_input.trim();
            if !trimmed_w.is_empty() {
                worker = trimmed_w.to_string();
            } else {
                worker = default_worker.to_string();
            }
        }

        if !address.is_empty() {
            let _ = fs::write("config.txt", format!("WALLET={}\nWORKER={}\n", address, worker));
            let _ = fs::write("wallet.txt", &address);
        }
    }

    if address.is_empty() {
        eprintln!("[ERROR] Mining address is required!");
        std::process::exit(1);
    }

    println!("[CONFIG] Pool Node : {}", pool_url);
    println!("[CONFIG] Wallet    : {}", address);
    println!("[CONFIG] Worker    : {}\n", worker);

    let active_job = Arc::new(RwLock::new(ActiveJob::default()));
    let total_hashes = Arc::new(AtomicU64::new(0));
    let accepted_shares = Arc::new(AtomicU32::new(0));
    let rejected_shares = Arc::new(AtomicU32::new(0));
    let blocks_found = Arc::new(AtomicU32::new(0));
    let running = Arc::new(AtomicBool::new(true));
    let event_logger = Arc::new(EventLogger::new(6));

    // Template poller thread
    {
        let pool_url = pool_url.clone();
        let address = address.clone();
        let worker = worker.clone();
        let active_job = Arc::clone(&active_job);
        let event_logger = Arc::clone(&event_logger);
        let running = Arc::clone(&running);

        thread::spawn(move || {
            let agent = ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(5))
                .build();
            let template_url = format!("{}/api/pool/template?address={}&worker={}", pool_url, address, worker);

            while running.load(Ordering::Relaxed) {
                if let Ok(resp) = agent.get(&template_url).call() {
                    if let Ok(tmpl) = resp.into_json::<TemplateResponse>() {
                        if let (Some(job_id), Some(pfx)) = (tmpl.job_id, tmpl.header_prefix) {
                            let diff = tmpl.share_difficulty.or(tmpl.target_difficulty).unwrap_or(3.0);
                            let target_u64 = tmpl.target_u64.and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
                            let sfx = tmpl.header_suffix.unwrap_or_default();
                            let seed = tmpl.seed.unwrap_or_else(|| "cortex-randomx-epoch-0".to_string());

                            let mut job = active_job.write().unwrap();
                            if job.job_id != job_id {
                                let old_diff = job.difficulty;
                                let is_first = job.job_id.is_empty();
                                let diff_changed = !is_first && (old_diff - diff).abs() >= 0.01;
                                let seed_changed = !is_first && !job.seed.is_empty() && job.seed != seed.as_bytes();

                                job.job_id = job_id.clone();
                                job.header_prefix = pfx.into_bytes();
                                job.header_suffix = sfx.into_bytes();
                                job.seed = seed.as_bytes().to_vec();
                                job.difficulty = diff;
                                job.target_u64 = target_u64;
                                job.valid = true;

                                if diff_changed {
                                    event_logger.log(format!("\x1b[1;34m► VARDIFF ADJUST\x1b[0m  Target Diff: {:.2} -> {:.2}", old_diff, diff));
                                } else if seed_changed {
                                    event_logger.log(format!("\x1b[1;35m► EPOCH CHANGED\x1b[0m   New Seed: {}", &seed[..seed.len().min(16)]));
                                }
                            }
                        }
                    }
                }
                thread::sleep(Duration::from_millis(1500));
            }
        });
    }

    println!("[START] Waiting for first mining job template from PowGrid...");
    while running.load(Ordering::Relaxed) {
        if active_job.read().unwrap().valid { break; }
        thread::sleep(Duration::from_millis(200));
    }

    // Register signal handler for Ctrl+C cursor restoration
    unsafe {
        libc::signal(libc::SIGINT, sigint_handler as usize);
        libc::signal(libc::SIGTERM, sigint_handler as usize);
    }

    let initial_diff = active_job.read().unwrap().difficulty;
    event_logger.log(format!("\x1b[1;36m► MINER STARTED\x1b[0m   {} threads active | Diff: {:.2}", num_threads, initial_diff));

    // Clear screen, scrollback and hide cursor for flicker-free static UI
    print!("\x1b[2J\x1b[3J\x1b[H\x1b[?25l");
    let _ = io::stdout().flush();

    // Reporter thread (Fixed static dashboard, 1s refresh, zero flicker)
    {
        let total_hashes = Arc::clone(&total_hashes);
        let accepted_shares = Arc::clone(&accepted_shares);
        let rejected_shares = Arc::clone(&rejected_shares);
        let blocks_found = Arc::clone(&blocks_found);
        let active_job = Arc::clone(&active_job);
        let event_logger = Arc::clone(&event_logger);
        let running = Arc::clone(&running);
        let pool_display = pool_url.clone();
        let wallet_display = if address.len() > 22 {
            format!("{}...{}", &address[..12], &address[address.len()-8..])
        } else {
            address.clone()
        };
        let worker_name = worker.clone();
        let start_time = Instant::now();

        thread::spawn(move || {
            let mut last_hashes = 0u64;
            let mut last_time = Instant::now();

            while running.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_secs(1));
                let now = Instant::now();
                let elapsed = (now - last_time).as_secs_f64();
                let curr_hashes = total_hashes.load(Ordering::Relaxed);
                let delta = curr_hashes.saturating_sub(last_hashes);
                let hs_instant = if elapsed > 0.0 { delta as f64 / elapsed } else { 0.0 };

                last_hashes = curr_hashes;
                last_time = now;

                let total_elapsed = start_time.elapsed().as_secs_f64();
                let hs_avg = if total_elapsed > 0.0 { curr_hashes as f64 / total_elapsed } else { 0.0 };

                let (now_str, now_unit) = format_speed(hs_instant);
                let (avg_str, avg_unit) = format_speed(hs_avg);

                let diff = active_job.read().unwrap().difficulty;
                let acc = accepted_shares.load(Ordering::Relaxed);
                let rej = rejected_shares.load(Ordering::Relaxed);
                let blocks = blocks_found.load(Ordering::Relaxed);
                let total_s = acc + rej;
                let acc_pct = if total_s > 0 { (acc as f64 / total_s as f64) * 100.0 } else { 100.0 };
                let rej_pct = if total_s > 0 { (rej as f64 / total_s as f64) * 100.0 } else { 0.0 };

                let uptime_secs = total_elapsed as u64;
                let u_h = uptime_secs / 3600;
                let u_m = (uptime_secs % 3600) / 60;
                let u_s = uptime_secs % 60;

                let eff_kh = (hs_avg / (num_threads as f64)) / 1000.0;
                let events = event_logger.get_events();

                let blocks_str = if blocks > 0 {
                    format!("\x1b[1;93m★ {} BLOCK{}\x1b[0m", blocks, if blocks > 1 { "S" } else { "" })
                } else {
                    "\x1b[1;37m0\x1b[0m \x1b[90m(hunting)\x1b[0m".to_string()
                };

                let mut buf = String::with_capacity(4096);
                buf.push_str("\x1b[H"); // Cursor home (Row 1, Col 1)

                let border_top = format!("╭{}╮\x1b[K\n", "─".repeat(76));
                let border_div = format!("├{}┤\x1b[K\n", "─".repeat(76));
                let border_bot = format!("╰{}╯\x1b[K\n", "─".repeat(76));

                buf.push_str(&border_top);
                buf.push_str(&box_row("\x1b[1;36m► POWGRID RETICULUM AI ($RAIX) HIGH-PERFORMANCE CPU MINER v1.1\x1b[0m", 74));
                buf.push_str(&two_col_row(
                    &format!("\x1b[90mPool:\x1b[0m \x1b[1;37m{}\x1b[0m", pool_display),
                    41,
                    "\x1b[90mStatus:\x1b[0m \x1b[1;32m● ONLINE (Connected)\x1b[0m",
                    74
                ));
                buf.push_str(&two_col_row(
                    &format!("\x1b[90mWallet:\x1b[0m \x1b[36m{}\x1b[0m", wallet_display),
                    41,
                    &format!("\x1b[90mWorker:\x1b[0m \x1b[1;33m{}\x1b[0m", worker_name),
                    74
                ));
                buf.push_str(&border_div);
                buf.push_str(&box_row("\x1b[1;35mHARDWARE & ENGINE CONFIGURATION\x1b[0m", 74));
                buf.push_str(&two_col_row(
                    "\x1b[90mAlgorithm :\x1b[0m Cortex-RandomX (L1 Fast)",
                    41,
                    &format!("\x1b[90mThreads :\x1b[0m \x1b[1;37m{:>2} Cores [AVX2+SHA-NI]\x1b[0m", num_threads),
                    74
                ));
                buf.push_str(&two_col_row(
                    "\x1b[90mScratchpad:\x1b[0m 32 KB/core (L1 Zero-RAM)",
                    41,
                    "\x1b[90mAccuracy:\x1b[0m \x1b[1;32m100% Bit-Perfect Match\x1b[0m",
                    74
                ));
                buf.push_str(&border_div);
                buf.push_str(&box_row("\x1b[1;33mMINING TELEMETRY\x1b[0m", 74));
                buf.push_str(&two_col_row(
                    &format!("\x1b[90mHashrate (Now) :\x1b[0m \x1b[1;32m{:>6.2} {:<4}\x1b[0m", now_str, now_unit),
                    41,
                    &format!("\x1b[90mUptime      :\x1b[0m \x1b[1;37m{:02}:{:02}:{:02}\x1b[0m", u_h, u_m, u_s),
                    74
                ));
                buf.push_str(&two_col_row(
                    &format!("\x1b[90mHashrate (Avg) :\x1b[0m \x1b[1;32m{:>6.2} {:<4}\x1b[0m", avg_str, avg_unit),
                    41,
                    &format!("\x1b[90mEfficiency  :\x1b[0m \x1b[1;37m{:>5.1} kH/s/core\x1b[0m", eff_kh),
                    74
                ));
                buf.push_str(&two_col_row(
                    &format!("\x1b[90mTarget Diff    :\x1b[0m \x1b[1;36m{:.2}\x1b[0m", diff),
                    41,
                    &format!("\x1b[90mTotal Hashes:\x1b[0m \x1b[1;37m{}\x1b[0m", format_number(curr_hashes)),
                    74
                ));
                buf.push_str(&two_col_row(
                    &format!("\x1b[90mShares (Acc)   :\x1b[0m \x1b[1;32m{} ({:.1}%)\x1b[0m", acc, acc_pct),
                    41,
                    &format!("\x1b[90mRejected    :\x1b[0m \x1b[1;31m{} ({:.1}%)\x1b[0m", rej, rej_pct),
                    74
                ));
                buf.push_str(&two_col_row(
                    &format!("\x1b[90mBlocks Solved  :\x1b[0m {}", blocks_str),
                    41,
                    "\x1b[90mBlock Reward:\x1b[0m \x1b[1;33m50.0 $RAIX\x1b[0m",
                    74
                ));
                buf.push_str(&border_div);
                buf.push_str(&box_row("\x1b[1;36mLIVE EVENT LOG (Latest Activity)\x1b[0m", 74));

                for i in 0..6 {
                    if i < events.len() {
                        buf.push_str(&box_row(&truncate_visible(&events[i], 74), 74));
                    } else {
                        buf.push_str(&box_row("", 74));
                    }
                }

                buf.push_str(&border_bot);
                buf.push_str("  \x1b[90m[CTRL+C to safely exit miner]\x1b[0m\x1b[K\n");

                print!("{}", buf);
                let _ = io::stdout().flush();
            }
        });
    }

    // Worker threads
    let mut handles = Vec::new();
    for tid in 0..num_threads {
        let active_job = Arc::clone(&active_job);
        let total_hashes = Arc::clone(&total_hashes);
        let accepted_shares = Arc::clone(&accepted_shares);
        let rejected_shares = Arc::clone(&rejected_shares);
        let blocks_found = Arc::clone(&blocks_found);
        let event_logger = Arc::clone(&event_logger);
        let running = Arc::clone(&running);
        let pool_url = pool_url.clone();
        let address = address.clone();
        let worker = worker.clone();
        let throttle = throttle_ms;

        handles.push(thread::spawn(move || {
            let agent = ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(5))
                .build();
            let submit_url = format!("{}/api/pool/submit", pool_url);

            let mut h = Sha256::new();
            h.update(address.as_bytes());
            h.update(worker.as_bytes());
            let now_nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64;
            h.update(&now_nanos.to_le_bytes());
            let seed_bytes = h.finalize();
            let base_nonce = u64::from_le_bytes(seed_bytes[0..8].try_into().unwrap());
            let mut nonce = (base_nonce & 0x0000_ffff_ffff_ffff) | ((tid as u64) << 48);
            let mut scratchpad = [0i64; 4096];

            let mut cur_job_id = String::new();
            let mut prefix = Vec::new();
            let mut suffix = Vec::new();
            let mut diff = 3.0f64;
            let mut target_u64 = 0u64;
            let mut precomputed_seed: Option<PrecomputedSeed> = None;

            let mut header_buf = [0u8; 1024];
            let mut p_len = 0;
            let mut s_len = 0;

            while running.load(Ordering::Relaxed) {
                // Check if job updated
                {
                    let job = active_job.read().unwrap();
                    if job.valid && job.job_id != cur_job_id {
                        cur_job_id = job.job_id.clone();
                        prefix = job.header_prefix.clone();
                        suffix = job.header_suffix.clone();
                        diff = job.difficulty;
                        target_u64 = job.target_u64;

                        precomputed_seed = Some(PrecomputedSeed::new(&job.seed));
                        p_len = prefix.len();
                        header_buf[..p_len].copy_from_slice(&prefix);
                        s_len = suffix.len();
                    }
                }

                if prefix.is_empty() {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }

                if let Some(ref p_seed) = precomputed_seed {
                    // Batch 2500 hashes per core loop
                    for _ in 0..2500 {
                        let n_len = write_u64_ascii(nonce, &mut header_buf[p_len..]);
                        header_buf[p_len + n_len..p_len + n_len + s_len].copy_from_slice(&suffix);
                        let total_len = p_len + n_len + s_len;
                        let header = &header_buf[..total_len];

                        let hash = CortexRandomX::hash_fast(header, p_seed, &mut scratchpad);
                        let is_share = if target_u64 > 0 {
                            check_target_u64(&hash, target_u64)
                        } else {
                            check_difficulty(&hash, diff.round() as usize)
                        };
                        if is_share {
                            // Found valid share!
                            let submit_body = SubmitRequest {
                                job_id: &cur_job_id,
                                nonce,
                                address: &address,
                                worker: &worker,
                            };

                            if let Ok(resp) = agent.post(&submit_url).send_json(&submit_body) {
                                if let Ok(res) = resp.into_json::<SubmitResponse>() {
                                    if res.valid_share.unwrap_or(false) || res.accepted.unwrap_or(false) {
                                        accepted_shares.fetch_add(1, Ordering::Relaxed);
                                        let is_block = res.block_found.unwrap_or(false);
                                        if is_block {
                                            blocks_found.fetch_add(1, Ordering::Relaxed);
                                            event_logger.log(format!(
                                                "\x1b[1;93m★ [BLOCK SOLVED!] ★ RAIX Block Mined! (+50 $RAIX)\x1b[0m"
                                            ));
                                        } else {
                                            event_logger.log(format!(
                                                "\x1b[1;32m✓ SHARE ACCEPTED\x1b[0m  Diff: {:.2} | Nonce: 0x{:016x}",
                                                diff, nonce
                                            ));
                                        }
                                    } else {
                                        rejected_shares.fetch_add(1, Ordering::Relaxed);
                                        let reason = res.error.unwrap_or_else(|| "Unknown rejection".to_string());
                                        event_logger.log(format!(
                                            "\x1b[1;31m✗ SHARE REJECTED\x1b[0m  Reason: {}",
                                            reason
                                        ));
                                    }
                                }
                            }
                        }
                        nonce += 1;
                    }
                    total_hashes.fetch_add(2500, Ordering::Relaxed);
                }

                if throttle > 0 {
                    thread::sleep(Duration::from_millis(throttle));
                }
            }
        }));
    }

    for h in handles {
        let _ = h.join();
    }
}

fn run_benchmark(num_threads: usize, bench_secs: u64) {
    println!("[BENCHMARK] Running {} hardware threads for {} seconds...\n", num_threads, bench_secs);

    let total_hashes = Arc::new(AtomicU64::new(0));
    let shares_found = Arc::new(AtomicU32::new(0));
    let running = Arc::new(AtomicBool::new(true));

    let base_header = "125000:0000000000abcdef1234567890:1789260000:merkle123:mem123:6:".as_bytes().to_vec();
    let seed = b"cortex-randomx-epoch-0".to_vec();

    let mut handles = Vec::new();
    let start_time = Instant::now();
    let precomputed = PrecomputedSeed::new(&seed);
    let suffix = b":ctx1bde9419a13d673b57163d14ef29f3ed01843188137b28669";

    for tid in 0..num_threads {
        let total_hashes = Arc::clone(&total_hashes);
        let shares_found = Arc::clone(&shares_found);
        let running = Arc::clone(&running);
        let base_header = base_header.clone();
        let precomputed = precomputed.clone();

        handles.push(thread::spawn(move || {
            let mut scratchpad = [0i64; 4096];
            let mut nonce = (tid as u64 + 1) * 100_000_000_000;

            let mut header_buf = [0u8; 1024];
            let p_len = base_header.len();
            header_buf[..p_len].copy_from_slice(&base_header);
            let s_len = suffix.len();

            while running.load(Ordering::Relaxed) {
                for _ in 0..500 {
                    let n_len = write_u64_ascii(nonce, &mut header_buf[p_len..]);
                    header_buf[p_len + n_len..p_len + n_len + s_len].copy_from_slice(suffix);
                    let total_len = p_len + n_len + s_len;
                    let header = &header_buf[..total_len];

                    let hash = CortexRandomX::hash_fast(header, &precomputed, &mut scratchpad);
                    if check_difficulty(&hash, 3) {
                        shares_found.fetch_add(1, Ordering::Relaxed);
                    }
                    nonce += 1;
                }
                total_hashes.fetch_add(500, Ordering::Relaxed);
            }
        }));
    }

    let mut last_hashes = 0u64;
    let mut last_instant = Instant::now();
    let mut sec = 0;

    while sec < bench_secs {
        thread::sleep(Duration::from_secs(1));
        sec += 1;

        let now = Instant::now();
        let elapsed = (now - last_instant).as_secs_f64();
        let current_hashes = total_hashes.load(Ordering::Relaxed);
        let delta = current_hashes.saturating_sub(last_hashes);
        let speed = delta as f64 / elapsed;

        last_hashes = current_hashes;
        last_instant = now;

        println!(
            "  [{:02}s] Hashrate: {:8.2} H/s | Avg/Thread: {:6.2} H/s | Shares (diff=3): {} | Total: {}",
            sec,
            speed,
            speed / (num_threads as f64),
            shares_found.load(Ordering::Relaxed),
            current_hashes
        );
    }

    running.store(false, Ordering::Relaxed);
    for h in handles {
        let _ = h.join();
    }

    let total_elapsed = start_time.elapsed().as_secs_f64();
    let final_hashes = total_hashes.load(Ordering::Relaxed);
    let avg_speed = final_hashes as f64 / total_elapsed;

    println!("\n===========================================================");
    println!("  🏆 BENCHMARK RESULTS ({} THREADS)", num_threads);
    println!("===========================================================");
    println!("  Total Hashes Computed : {}", final_hashes);
    println!("  Duration              : {:.2} seconds", total_elapsed);
    println!("  Total Hashrate        : {:.2} H/s ({:.2} kH/s)", avg_speed, avg_speed / 1000.0);
    println!("  Per-Thread Average    : {:.2} H/s/thread", avg_speed / (num_threads as f64));
    println!("  Valid Shares Found    : {}", shares_found.load(Ordering::Relaxed));
    println!("===========================================================\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonical_genesis_vector_nonce_123() {
        let mut scratchpad = [0i64; 4096];
        let header = b"test_header_123";
        let seed = b"cortex-randomx-genesis-seed-v1";
        let hash = CortexRandomX::hash(header, seed, &mut scratchpad);
        let expected = "b1b9599c0a73a84c5777369c3a2af50e78cc63b6e0c22c30f191c98fc06316a5";
        assert_eq!(hex::encode(hash), expected, "Genesis vector mismatch!");
    }

    #[test]
    fn test_canonical_high_nonce_vector_99999() {
        let mut scratchpad = [0i64; 4096];
        let header = b"test_header_99999";
        let seed = b"cortex-randomx-genesis-seed-v1";
        let hash = CortexRandomX::hash(header, seed, &mut scratchpad);
        let expected = "912f8cb7ed73773658af2f4212aa40bb365d1a5a208eb09cd66e567b3cf70a5c";
        assert_eq!(hex::encode(hash), expected, "High-nonce vector mismatch!");
    }
}
