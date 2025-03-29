#![allow(unused_imports, dead_code, unused_variables)]

use libafl::{
    corpus::{Corpus, InMemoryCorpus, Testcase, CorpusId},
    feedbacks::MaxMapFeedback,
    inputs::{BytesInput, HasTargetBytes},
    observers::{CanTrack, MapObserver},
    schedulers::{ProbabilitySamplingScheduler, probabilistic_sampling::ProbabilityMetadata, Scheduler, TestcaseScore},
    state::{HasCorpus, StdState}, HasMetadata, HasNamedMetadata, Error
};
use libafl_bolts::{
    rands::RomuDuoJrRand,
    shmem::{MmapShMemProvider, ShMemId, ShMemProvider},
    AsSliceMut, AsSlice, HasLen, Named, impl_serdeany
};
use serde::{Deserialize, Serialize};
use std::{
    borrow::{Cow, BorrowMut},
    collections::HashMap,
    fs,
    hash::{Hash, Hasher},
    io::{self},
    thread,
    time::Duration,
};

/// **Uniform Probability Distribution for Sampling Scheduler**
#[derive(Debug, Clone)]
pub struct UniformDistribution {}

impl<S> TestcaseScore<BytesInput, S> for UniformDistribution
where
    S: HasCorpus<BytesInput> + HasMetadata + HasNamedMetadata,
{
    fn compute(state: &S, testcase: &mut Testcase<BytesInput>) -> Result<f64, Error> {
        // Attempt to fetch the observer, return 0.25 if it fails
        let observer = match state.metadata_map().get::<FuzzilliCoverageObserver>() {
            Some(obs) => obs,
            None => {
                println!("\n\nPROBLEM IN THE COMPUTE FUNCTION WHILE FETCHING FuzzilliCoverageObserver\n\n");
                return Ok(0.25);
            }
        };

        // Attempt to get input length, return 0.25 if there is an error
        let input_length = match testcase.input() {
            Some(input) => input.len() as f64,
            None => {
                println!("\n\nPROBLEM IN THE COMPUTE FUNCTION WHILE GETTING INPUT LENGTH\n\n");
                return Ok(0.25);
            }
        };

        let coverage_count = observer.count_bytes() as f64;
        
        // Compute the score
        let score = (coverage_count * 10.0) - (input_length * 0.1);
        
        // Ensure minimum score of 1.0
        Ok(score.max(1.0))
    }
}

pub type UniformProbabilitySamplingScheduler =
    ProbabilitySamplingScheduler<UniformDistribution>;

// **Custom Observer for Fuzzilli's Bit-Level Shared Memory Layout**
#[derive(Debug, Serialize, Deserialize)]
pub struct FuzzilliCoverageObserver {
    name: Cow<'static, str>,
    #[serde(skip)]
    map: Vec<u8>,  // Store memory directly (no Arc<Mutex<>>)
    num_edges: usize,
    initial: u8,
}
impl_serdeany!(FuzzilliCoverageObserver);

impl FuzzilliCoverageObserver {
    pub fn new(name: &'static str, map: Vec<u8>) -> Self {
        if map.len() < 4 {
            panic!("Shared memory too small to contain header!");
        }

        let num_edges = u32::from_le_bytes(map[0..4].try_into().expect("Line 67 lib.rs failed")) as usize;

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

impl Named for FuzzilliCoverageObserver {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl HasLen for FuzzilliCoverageObserver {
    fn len(&self) -> usize {
        self.num_edges
    }
}

// ** FIX: Implement AsRef & AsMut required by MapObserver **
impl AsRef<Self> for FuzzilliCoverageObserver {
    fn as_ref(&self) -> &Self {
        self
    }
}

impl AsMut<Self> for FuzzilliCoverageObserver {
    fn as_mut(&mut self) -> &mut Self {
        self
    }
}

impl MapObserver for FuzzilliCoverageObserver {
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
        self.num_edges
    }

    fn count_bytes(&self) -> u64 {
        self.map.iter().map(|&byte| byte.count_ones() as u64).sum()
    }

    fn reset_map(&mut self) -> Result<(), libafl::Error> {
        self.map.fill(0);
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

fn update_corpus(
    corpus_dir: &str,
    corpus: &mut InMemoryCorpus<BytesInput>,
    seen_inputs: &mut HashMap<Vec<u8>, bool>,
) -> Vec<CorpusId> {
    let mut corpus_ids = Vec::new();
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

                // Add testcase to the corpus and get the corpus ID
                let idx = corpus.add(testcase).unwrap();
                corpus_ids.push(idx);
                seen_inputs.insert(bytes, true);
                println!("Added new input from {:?}", path);
            }
        }
    }

    corpus_ids
}

fn update_scheduler(
    scheduler: &mut ProbabilitySamplingScheduler<UniformDistribution>,
    state: &mut StdState<InMemoryCorpus<BytesInput>, BytesInput, RomuDuoJrRand, InMemoryCorpus<BytesInput>>,
    corpus_ids: Vec<CorpusId>,
) {
    for idx in corpus_ids {
        println!("Adding testcase ID: {:?}", idx);
        scheduler.on_add(state.borrow_mut(), idx).unwrap();
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

    let mut shared_mem_clone = shmem.as_slice().to_vec(); // Clone to avoid borrow conflicts

    let raw_observer = FuzzilliCoverageObserver::new("fuzzilli_coverage", shared_mem_clone.clone());
    let observer = raw_observer.track_indices();

    let mut feedback = MaxMapFeedback::new(&observer);
    let mut objective_feedback = MaxMapFeedback::new(&observer);

    let corpus_dir = "../fuzzilli/sm_qss_out/pcorpus";
    let mut input_corpus = InMemoryCorpus::new();
    let mut seen_inputs: HashMap<Vec<u8>, bool> = HashMap::new();
    let corpus_ids = update_corpus(corpus_dir, &mut input_corpus, &mut seen_inputs);
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

    println!("State created successfully!");
    state.metadata_map_mut().insert(FuzzilliCoverageObserver::new("fuzzilli_coverage", shared_mem_clone.clone()));

    let mut scheduler = UniformProbabilitySamplingScheduler::new();

    update_scheduler(&mut scheduler, &mut state, corpus_ids);

    println!("Starting input suggestion loop...");
    loop {
        // let corpus_ids = update_corpus(corpus_dir, &mut input_corpus.clone(), &mut seen_inputs);
        // update_scheduler(&mut scheduler, &mut state, corpus_ids);

        // Debug: Ensure corpus is not empty before calling scheduler
        if state.corpus().count() == 0 {
            println!("Corpus is unexpectedly empty!");
        } else {
            println!("Corpus contains {} entries", state.corpus().count());
        }

        if state.corpus().count() == 0 {
            println!("Corpus is empty! No inputs to schedule.");
            continue;
        }

        let meta = state.metadata_map().get::<ProbabilityMetadata>().unwrap();
        if meta.map.is_empty() {
            println!("ProbabilityMetadata is empty!");
        } else {
            println!("ProbabilityMetadata has {} keys", meta.map.len());
        }

        println!("Metadata map contents: {:?}", state.metadata_map().len());
        match scheduler.next(&mut state) {
            Ok(next_id) => {
                println!("Scheduled testcase ID: {:?}", next_id);
                thread::sleep(Duration::from_millis(100));
            }
            Err(err) => {
                println!("Error while scheduling testcase: {:?}", err);
            }
        }
    }
}
