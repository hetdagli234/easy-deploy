use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, read_keypair_file};
use std::fs;
use std::path::{Path, PathBuf};
use std::fs::File;
use std::io::Read;
use solana_sdk::transaction::Transaction;
use solana_sdk::instruction::Instruction;
use solana_client::rpc_client::RpcClient;
use solana_sdk::signer::Signer;
use solana_sdk::message::Message;
use solana_sdk::commitment_config::CommitmentConfig;
use std::thread;
use std::time::Duration;
use log::{info, error, warn};
use env_logger;
use structopt::StructOpt;
use indicatif::{ProgressBar, ProgressStyle};

const MAX_CHUNK_SIZE: usize = 1000; 
const MAX_RETRIES: u32 = 5;
const RETRY_DELAY_MS: u64 = 5000;

struct BufferAccountManager {
    buffer_keypair: Keypair,
}

impl BufferAccountManager {
    fn new() -> Self {
        // Try to load existing buffer keypair or create a new one
        let buffer_keypair = match read_keypair_file("buffer_keypair.json") {
            Ok(keypair) => keypair,
            Err(_) => {
                let new_keypair = Keypair::new();
                let keypair_bytes = new_keypair.to_bytes();
                fs::write("buffer_keypair.json", keypair_bytes).expect("Failed to save buffer keypair");
                new_keypair
            }
        };
        
        BufferAccountManager { buffer_keypair }
    }

    fn get_buffer_pubkey(&self) -> Pubkey {
        self.buffer_keypair.pubkey()
    }
}

struct Deployer {
    rpc_client: RpcClient,
    buffer_manager: BufferAccountManager,
    max_retries: u32,
    retry_delay: u64,
}

impl Deployer {
    fn new(rpc_url: &str, max_retries: u32, retry_delay: u64) -> Self {
        let rpc_client = RpcClient::new(rpc_url.to_string());
        let buffer_manager = BufferAccountManager::new();
        Deployer { rpc_client, buffer_manager, max_retries, retry_delay }
    }

    fn initialize_buffer_account(&self, program_len: usize) -> Result<(), Box<dyn std::error::Error>> {
        let buffer_pubkey = self.buffer_manager.get_buffer_pubkey();
        let payer = self.buffer_manager.buffer_keypair.pubkey();

        let rent = self.rpc_client.get_minimum_balance_for_rent_exemption(
            program_len,
        )?;

        let instruction = solana_program::bpf_loader_upgradeable::create_buffer(
            &payer,
            &buffer_pubkey,
            &payer,
            rent,
            program_len,
        )?;

        let transaction = self.create_and_sign_transaction(instruction)?;
        self.send_and_confirm_transaction(transaction)?;

        println!("Buffer account initialized: {}", buffer_pubkey);
        Ok(())
    }

    fn deploy(&self, program_data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        self.initialize_buffer_account(program_data.len())?;

        let total_chunks = (program_data.len() + MAX_CHUNK_SIZE - 1) / MAX_CHUNK_SIZE;
        let progress_bar = ProgressBar::new(total_chunks as u64);
        progress_bar.set_style(ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} chunks")
            .unwrap()
            .progress_chars("##-"));

        for (i, chunk) in program_data.chunks(MAX_CHUNK_SIZE).enumerate() {
            let offset = i * MAX_CHUNK_SIZE;
            let instruction = create_write_instruction(self.buffer_manager.get_buffer_pubkey(), offset, chunk.to_vec());
            let transaction = self.create_and_sign_transaction(vec![instruction])?;
            
            self.send_and_confirm_transaction(transaction)?;
            
            progress_bar.inc(1);
        }

        progress_bar.finish_with_message("Program chunks deployed successfully");

        let program_id = Pubkey::new_unique();
        self.finalize_deployment(program_id, program_data)?;

        Ok(())
    }

    fn create_and_sign_transaction(&self, instructions: Vec<Instruction>) -> Result<Transaction, Box<dyn std::error::Error>> {
        let recent_blockhash = self.rpc_client.get_latest_blockhash()?;
        let payer = self.buffer_manager.buffer_keypair.pubkey();
        
        let message = Message::new(&instructions, Some(&payer));
        let mut transaction = Transaction::new_unsigned(message);
        
        transaction.sign(&[&self.buffer_manager.buffer_keypair], recent_blockhash);
        
        Ok(transaction)
    }

    fn send_and_confirm_transaction(&self, transaction: Transaction) -> Result<(), Box<dyn std::error::Error>> {
        for attempt in 1..=self.max_retries {
            match self.rpc_client.send_and_confirm_transaction_with_spinner_and_config(
                &transaction,
                CommitmentConfig::confirmed(),
                solana_client::rpc_config::RpcSendTransactionConfig::default(),
            ) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    eprintln!("Transaction failed (attempt {}): {}", attempt, e);
                    if attempt < self.max_retries {
                        thread::sleep(Duration::from_millis(self.retry_delay));
                    }
                }
            }
        }
        Err("Max retries reached. Transaction failed.".into())
    }

    fn finalize_deployment(&self, program_id: Pubkey, program_data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        let buffer_pubkey = self.buffer_manager.get_buffer_pubkey();
        let payer = self.buffer_manager.buffer_keypair.pubkey();

        let instruction = solana_program::bpf_loader_upgradeable::deploy_with_max_program_len(
            &payer,
            &program_id,
            &buffer_pubkey,
            &payer,
            self.rpc_client.get_minimum_balance_for_rent_exemption(
                program_data.len(),
            )?,
            program_data.len(),
        )?;

        let transaction = self.create_and_sign_transaction(instruction)?;
        self.send_and_confirm_transaction(transaction)?;

        println!("Program deployment finalized. Program ID: {}", program_id);
        Ok(())
    }

    fn cleanup_unused_buffers(&self) -> Result<(), Box<dyn std::error::Error>> {
        // This is a simplified version. In a real implementation, you'd need to:
        // 1. Fetch all buffer accounts associated with the payer
        // 2. Check which ones are unused (not associated with any deployed program)
        // 3. Close those accounts and recover the rent

        info!("Cleaning up unused buffer accounts...");
        // Implementation details would go here
        info!("Cleanup completed");

        Ok(())
    }
}

fn create_write_instruction(buffer_pubkey: Pubkey, offset: usize, data: Vec<u8>) -> Instruction {
    solana_program::bpf_loader_upgradeable::write(
        &buffer_pubkey,
        &buffer_pubkey, 
        offset as u32,
        data,
    )
}

fn load_program(path: &Path) -> Result<Vec<u8>, std::io::Error> {
    let mut file = File::open(path)?;
    let mut program_data = Vec::new();
    file.read_to_end(&mut program_data)?;
    Ok(program_data)
}

#[derive(Debug, StructOpt)]
#[structopt(name = "ed", about = "Easy Deploy for Solana Programs")]
struct Opt {
    /// Path to the .so file
    #[structopt(parse(from_os_str))]
    so_file: PathBuf,

    /// RPC URL for Solana cluster
    #[structopt(long, default_value = "https://api.devnet.solana.com")]
    rpc_url: String,

    /// Maximum number of retries for failed transactions
    #[structopt(long, default_value = "5")]
    max_retries: u32,

    /// Delay between retries in milliseconds
    #[structopt(long, default_value = "5000")]
    retry_delay: u64,
}

fn main() {
    env_logger::init();
    let opt = Opt::from_args();

    // Use opt.so_file instead of args[1]
    let so_file_path = &opt.so_file;
    if !so_file_path.exists() || !so_file_path.is_file() {
        error!("Error: Invalid .so file path");
        std::process::exit(1);
    }

    info!("Deploying Solana program from: {}", so_file_path.display());
    let program_data = load_program(so_file_path);

    let program_data = match program_data {
        Ok(data) => data,
        Err(e) => {
            error!("Failed to load program: {}", e);
            std::process::exit(1);
        }
    };

    info!("Loaded program, size: {} bytes", program_data.len());

    let deployer = Deployer::new(
        &opt.rpc_url,
        opt.max_retries,
        opt.retry_delay,
    );
    match deployer.deploy(&program_data) {
        Ok(_) => {
            info!("Deployment completed successfully");
            if let Err(e) = deployer.cleanup_unused_buffers() {
                warn!("Failed to clean up unused buffers: {}", e);
            }
        },
        Err(e) => error!("Deployment failed: {}", e),
    }
}
