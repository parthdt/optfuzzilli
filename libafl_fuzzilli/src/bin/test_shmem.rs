#![allow(unused_imports, dead_code, unused_variables)]

use libafl::{
    corpus::{Corpus, InMemoryCorpus, Testcase},
    feedbacks::MaxMapFeedback,
    inputs::{BytesInput, HasTargetBytes},
    observers::{CanTrack, MapObserver},
    schedulers::{CoverageAccountingScheduler, IndexesLenTimeMinimizerScheduler, QueueScheduler, Scheduler},
    state::{HasCorpus, StdState},
};
use libafl_bolts::{
    rands::RomuDuoJrRand,
    shmem::{MmapShMemProvider, ShMemId, ShMemProvider},
    AsSliceMut, HasLen, Named,
};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::HashMap,
    fs,
    hash::{Hash, Hasher},
    io::{self},
    thread,
    time::Duration,
};

/// Custom observer for interpreting Fuzzilli's shared memory layout.
#[derive(Debug, Serialize, Deserialize, Hash)]
pub struct FuzzilliCoverageObserver<'a> {
    name: Cow<'static, str>,
    #[serde(skip)]
    map: &'a mut [u8],
    num_edges: usize, // Stores number of edges from shmem
    initial: u8,
}

impl<'a> FuzzilliCoverageObserver<'a> {
    pub fn new(name: &'static str, map: &'a mut [u8]) -> Self {
        if map.len() < 4 {
            panic!("Shared memory too small to contain header!");
        }

        // Extract the number of edges from the first 4 bytes
        let num_edges = u32::from_le_bytes(map[0..4].try_into().unwrap()) as usize;

        if map.len() < 4 + (num_edges / 8) {
            panic!("Shared memory does not contain enough coverage data!");
        }

        Self {
            name: Cow::from(name),
            map,
            num_edges,
            initial: 0,
        }
    }
}

impl<'a> Named for FuzzilliCoverageObserver<'a> {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<'a> HasLen for FuzzilliCoverageObserver<'a> {
    fn len(&self) -> usize {
        self.num_edges
    }
}

impl<'a> AsRef<Self> for FuzzilliCoverageObserver<'a> {
    fn as_ref(&self) -> &Self {
        &self
    }
}

impl<'a> AsMut<Self> for FuzzilliCoverageObserver<'a> {
    fn as_mut(&mut self) -> &mut Self {
        self
    }
}

impl<'a> MapObserver for FuzzilliCoverageObserver<'a> {
    type Entry = u8;

    fn get(&self, idx: usize) -> Self::Entry {
        if idx >= self.num_edges {
            0
        } else {
            let byte_idx = 4 + (idx / 8);
            let bit_idx = idx % 8;
            (self.map[byte_idx] >> bit_idx) & 1
        }
    }

    fn set(&mut self, idx: usize, value: Self::Entry) {
        if idx < self.num_edges {
            let byte_idx = 4 + (idx / 8);
            let bit_idx = idx % 8;
            if value != 0 {
                self.map[byte_idx] |= 1 << bit_idx;
            } else {
                self.map[byte_idx] &= !(1 << bit_idx);
            }
        }
    }

    fn usable_count(&self) -> usize {
        self.num_edges // Each bit represents an edge
    }

    fn count_bytes(&self) -> u64 {
        self.map[4..(4 + (self.num_edges / 8))]
            .iter()
            .map(|&byte| byte.count_ones() as u64)
            .sum()
    }

    fn reset_map(&mut self) -> Result<(), libafl::Error> {
        self.map[4..(4 + (self.num_edges / 8))].fill(0);
        Ok(())
    }

    fn initial(&self) -> Self::Entry {
        self.initial
    }

    fn to_vec(&self) -> Vec<Self::Entry> {
        let mut bits = Vec::with_capacity(self.num_edges);
        for idx in 0..self.num_edges {
            bits.push(self.get(idx));
        }
        bits
    }

    fn how_many_set(&self, indices: &[usize]) -> usize {
        indices.iter().filter(|&&idx| self.get(idx) > 0).count()
    }
}

/// Updates the corpus by adding new inputs from the directory while avoiding duplicates.
fn update_corpus(
    corpus_dir: &str,
    corpus: &mut InMemoryCorpus<BytesInput>,
    seen_inputs: &mut HashMap<Vec<u8>, bool>,
) {
    let entries = fs::read_dir(corpus_dir).expect("Failed to read input corpus directory");

    for entry in entries {
        let entry = entry.expect("Failed to read entry");
        let path = entry.path();

        if let Some(file_name) = path.file_name() {
            if file_name.to_string_lossy().starts_with('.') {
                continue;
            }
        }

        if path.is_file() {
            let bytes = fs::read(&path).expect("Failed to read file");
            if !seen_inputs.contains_key(&bytes) {
                let input = BytesInput::new(bytes.clone());
                let testcase = Testcase::new(input);
                corpus.add(testcase).expect("Failed to add testcase to corpus");
                seen_inputs.insert(bytes, true);
                println!("Added new input from {:?}", path);
            }
        }
    }
}

fn main() {
    let mut input = String::new();
    println!("Enter the shared memory key (e.g., shm_id_36095_0):");
    io::stdin()
        .read_line(&mut input)
        .expect("Failed to read input");
    let shm_key = input.trim();
    let shmem_id = ShMemId::from_string(shm_key);
    let mut shmem_provider = MmapShMemProvider::new().expect("Failed to create shared memory provider");

    println!("Attempting to attach to shared memory with key: {}", shm_key);
    let mut shmem = shmem_provider
        .shmem_from_id_and_size(shmem_id, 0x200000)
        .expect("Failed to attach to shared memory");

    let mut shared_mem_clone = shmem.as_slice_mut().to_vec(); // Clone to avoid borrow conflicts

    let observer = FuzzilliCoverageObserver::new("fuzzilli_coverage", shmem.as_slice_mut()).track_indices();

    let mut feedback = MaxMapFeedback::new(&observer);
    let mut objective_feedback = MaxMapFeedback::new(&observer);

    let corpus_dir = "../fuzzilli/pcorpus";
    let mut input_corpus = InMemoryCorpus::new();
    let mut seen_inputs: HashMap<Vec<u8>, bool> = HashMap::new();

    update_corpus(corpus_dir, &mut input_corpus, &mut seen_inputs);
    println!("Loaded {} inputs into the in-memory corpus.", input_corpus.count());

    let rng = RomuDuoJrRand::with_seed(12345);
    let solutions_corpus = InMemoryCorpus::new();
    let mut state = StdState::new(
        rng,
        input_corpus,
        solutions_corpus,
        &mut feedback,
        &mut objective_feedback,
    )
    .expect("Failed to create state");

    if shared_mem_clone.len() < 4 {
        panic!("Shared memory region is too small to contain header.");
    }
    
    let num_edges = u32::from_le_bytes(shared_mem_clone[0..4].try_into().unwrap()) as usize;
    println!("Number of edges in shared memory: {}", num_edges);
    let coverage_data = &shared_mem_clone[4..];
    
    if coverage_data.len() < num_edges {
        panic!("Shared memory does not contain enough coverage data for the declared number of edges.");
    }
    
    let accounting_map: Vec<u32> = coverage_data
        .iter()
        .take(num_edges)
        .map(|&byte| byte as u32)
        .collect();

    let mut scheduler = CoverageAccountingScheduler::new(&observer, &mut state, QueueScheduler::new(), &accounting_map);

    println!("Starting input suggestion loop...");
    loop {
        update_corpus(corpus_dir, state.corpus_mut(), &mut seen_inputs);

        let raw_observer = observer.as_ref();
        let map = &raw_observer.map;
        let total_entries = map.len();
        let total_non_zero: usize = map.iter().filter(|&&x| x > 0).count();
        let max_value = map.iter().max().unwrap_or(&0);

        println!("\n=== Fuzzilli Shared Memory Coverage ===");
        // println!("Non-Zero Entries (Covered): {}", total_non_zero);
        // println!("Max Value (Hit Count): {}", max_value);
        println!("Observer (First 512 Bytes): {:?}", &map[..512]);
        println!("Corpus size: {}", state.corpus().count());
        println!("Number of edges in shared memory: {}", num_edges);

        println!("Shared memory contents (first 128 bytes as bits):");
        for byte in &shared_mem_clone[..128.min(shared_mem_clone.len())] {
            print!("{:08b} ", byte);
        }
        println!();
        // println!("Accounting Map: {:?}", &accounting_map[..256]);
        match scheduler.next(&mut state) {
            Ok(next_id) => {
                println!("Scheduler selected input ID: {:?}", next_id);

                let next_testcase = state.corpus().get(next_id).unwrap();
                let next_input = next_testcase.borrow();
                let input_bytes = next_input.input().as_ref().unwrap();

                println!("Next input: {:?}", input_bytes.target_bytes());
            }
            Err(err) => {
                println!("Scheduler error: {:?}", err);
            }
        }

        thread::sleep(Duration::from_secs(5));
    }
}
