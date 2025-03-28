#![allow(unused_imports, dead_code, unused_variables)]

use std::sync::{Arc, Mutex};
use libafl::{
    corpus::{InMemoryCorpus, OnDiskCorpus, Testcase, Corpus, CorpusId},
    feedbacks::{MaxMapFeedback, DifferentIsNovel, MapFeedback, ConstFeedback},
    inputs::{BytesInput, HasMutatorBytes},
    observers::{CanTrack, MapObserver, ExplicitTracking},
    schedulers::{IndexesLenTimeMinimizerScheduler, QueueScheduler, ProbabilitySamplingScheduler, TestcaseScore, Scheduler, CoverageAccountingScheduler},
    state::{StdState, HasCorpus, HasSolutions},
    Error, HasMetadata, HasNamedMetadata
};
use libafl_bolts::{
    rands::RomuDuoJrRand,
    shmem::{MmapShMem, MmapShMemProvider, ShMemProvider, ShMemId},
    Named, HasLen, AsSliceMut, serdeany::SerdeAny, AsSlice, serdeany::RegistryBuilder,
};
use libafl_bolts::impl_serdeany;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::any::Any;

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

/// **Custom Observer for Fuzzilli's Bit-Level Shared Memory Layout**
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

#[derive(Debug)]
pub enum SchedulerEnum {
    UniformProbability(UniformProbabilitySamplingScheduler),
    Queue(QueueScheduler),
    CoverageAccounting(
        CoverageAccountingScheduler<
            'static,
            QueueScheduler,
            BytesInput,
            ExplicitTracking<FuzzilliCoverageObserver, true, false>,
        >,
    ),
    IndexesLenTimeMinimizer(
        IndexesLenTimeMinimizerScheduler<QueueScheduler, BytesInput, ExplicitTracking<FuzzilliCoverageObserver, true, false>>,
    ),
}
    

#[derive(uniffi::Object, Debug)]
pub struct LibAflObject {
    state: Arc<Mutex<StdState<OnDiskCorpus<BytesInput>, BytesInput, RomuDuoJrRand, InMemoryCorpus<BytesInput>>>>,
    scheduler: Arc<Mutex<SchedulerEnum>>,
    _shmem: Arc<Mutex<MmapShMem>>, // Keep shared memory alive
}

unsafe impl Send for LibAflObject {}
unsafe impl Sync for LibAflObject {}

#[uniffi::export]
impl LibAflObject {
    #[uniffi::constructor]
    pub fn new(corpus_dir: String, shmem_key: String, scheduler_type: u8) -> Arc<Self> {

        match scheduler_type {
            1 => println!("Using UniformProbabilityScheduler"),
            2 => println!("Using QueueScheduler"),
            3 => println!("Using CoverageAccountingScheduler"),
            4 => println!("Using IndexesLenTimeMinimizerScheduler"),
            _ => println!("Unknown scheduler type"),
        }

        let mut shmem_provider = MmapShMemProvider::new().expect("Failed to create shared memory provider, line 241 lib.rs failed");
        let shmem_id = ShMemId::from_string(&shmem_key);
        let shmem = shmem_provider
            .shmem_from_id_and_size(shmem_id, 0x200000)
            .expect("Failed to attach to shared memory, line 245 lib.rs failed");

        let shmem_arc = Arc::new(Mutex::new(shmem));

        let shared_mem_vec = {
            let shmem_locked = shmem_arc.lock().unwrap();
            shmem_locked.as_slice().to_vec()
        };

        let coverage_data = &shared_mem_vec[4..];
        let num_edges = u32::from_le_bytes(shared_mem_vec[0..4].try_into().expect("line 255 lib.rs failed")) as usize;
         // Create a clone of the slice for accounting map creation
        let accounting_map: Vec<u32> = coverage_data
        .iter()
        .take(num_edges)
        .map(|&byte| byte as u32)
        .collect();

        let raw_observer = FuzzilliCoverageObserver::new("fuzzilli_coverage", shared_mem_vec.clone());
        let observer_clone = FuzzilliCoverageObserver::new("fuzzilli_coverage", shared_mem_vec.clone());
        let observer = raw_observer.track_indices();

        let on_disk_corpus = OnDiskCorpus::<BytesInput>::new(&corpus_dir).expect("Failed to create OnDiskCorpus, line 267 lib.rs failed");
        let in_memory_corpus = InMemoryCorpus::<BytesInput>::new();

        let rng = RomuDuoJrRand::with_seed(12345);

        let mut feedback = MaxMapFeedback::new(&observer);
        let mut objective_feedback = ConstFeedback::new(false);

        let mut state = StdState::new(
            rng,
            on_disk_corpus,
            in_memory_corpus,
            &mut feedback,
            &mut objective_feedback,
        )
        .expect("Failed to initialize StdState, line 282 lib.rs failed");

        // Now we can insert the observer
        state.metadata_map_mut().insert(observer_clone);
        
        let scheduler = match scheduler_type {
            1 => SchedulerEnum::UniformProbability(UniformProbabilitySamplingScheduler::new()),
            2 => SchedulerEnum::Queue(QueueScheduler::new()),
            3 => SchedulerEnum::CoverageAccounting(CoverageAccountingScheduler::new(
                &observer,
                &mut state,
                QueueScheduler::new(),
                Box::leak(accounting_map.into_boxed_slice()),
            )),
            4 => SchedulerEnum::IndexesLenTimeMinimizer(IndexesLenTimeMinimizerScheduler::new(
                &observer,
                QueueScheduler::new(),
            )),
            _ => panic!("Invalid scheduler type! Use 1, 2, 3, or 4. Line 300 lib.rs failed"),
        };

        Arc::new(Self {
            state: Arc::new(Mutex::new(state)),
            scheduler: Arc::new(Mutex::new(scheduler)),
            _shmem: shmem_arc,
        })
    }

    /// Add a new input to the corpus.
    pub fn add_input(&self, input_data: Vec<u8>) {
        let input = BytesInput::new(input_data);
        let testcase = Testcase::new(input);
        let mut state = self.state.lock().unwrap();
        
        // Add the input to the corpus and get the index
        let idx = state.corpus_mut().add(testcase).expect("Failed to add testcase to corpus");
        
        // Check the scheduler type and call on_add if UniformProbability
        match &mut *self.scheduler.lock().unwrap() {
            SchedulerEnum::UniformProbability(s) => s.on_add(&mut *state, idx).unwrap(),
            _ => {}, // For other schedulers, no need to call on_add
        }
        
        let cur_count = state.solutions().count() as u64;
        // println!("Added input to corpus. Current count of solutions corpus: {}", cur_count);
    }
    

    pub fn suggest_next_input(&self) -> Vec<u8> {
        let mut scheduler = self.scheduler.lock().unwrap();
        let mut state = self.state.lock().unwrap();

        let next_id = match &mut *scheduler {
            SchedulerEnum::UniformProbability(s) => s.next(&mut *state),
            SchedulerEnum::Queue(s) => s.next(&mut *state),
            SchedulerEnum::CoverageAccounting(s) => s.next(&mut *state),
            SchedulerEnum::IndexesLenTimeMinimizer(s) => s.next(&mut *state),
        }.expect("Failed to fetch next input ID, line 329 lib.rs failed");
        // let next_id = scheduler.next(&mut *state).expect("Failed to fetch next input ID");
        let testcase = state.corpus().get(next_id).unwrap();
        let borrowed = testcase.borrow();
        let input = borrowed.input().as_ref().unwrap();
        input. mutator_bytes().to_vec()
    }

    pub fn count(&self) -> u64 {
        let state = self.state.lock().unwrap();
        state.corpus().count() as u64
    }

    pub fn first_index(&self) -> u64 {
        let state = self.state.lock().unwrap();
        let first_id = state.corpus().first().unwrap_or(CorpusId(0));
        let first_id_usize : usize = first_id.into();
        let first_id_u64 : u64 = first_id_usize as u64;
        first_id_u64
    }

    pub fn last_index(&self) -> u64 {
        let state = self.state.lock().unwrap();
        let last_id = state.corpus().last().unwrap_or(CorpusId(0));
        let last_id_usize : usize = last_id.into();
        let last_id_u64 : u64 = last_id_usize as u64;
        last_id_u64
    }

    pub fn get_element(&self, id: u64) -> Vec<u8> {
        let state = self.state.lock().unwrap();
        let corpus_id = CorpusId(id as usize);
        match state.corpus().get(corpus_id) {
            Ok(testcase) => {
                if let Some(input) = testcase.borrow().input() {
                    input.mutator_bytes().to_vec()
                } else {
                    Vec::new()
                }
            }
            Err(_) => Vec::new(),
        }
    }
}

uniffi::setup_scaffolding!();
