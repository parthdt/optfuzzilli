#![allow(unused_imports, dead_code, unused_variables)]

use std::sync::{Arc, Mutex};
use libafl::{
    corpus::{InMemoryCorpus, OnDiskCorpus, Testcase, Corpus, CorpusId},
    feedbacks::{MaxMapFeedback, DifferentIsNovel, MapFeedback},
    inputs::{BytesInput, HasMutatorBytes},
    observers::{CanTrack, MapObserver, ExplicitTracking},
    schedulers::{IndexesLenTimeMinimizerScheduler, QueueScheduler, ProbabilitySamplingScheduler, TestcaseScore, Scheduler, CoverageAccountingScheduler},
    state::{StdState, HasCorpus},
    Error,
};
use libafl_bolts::{
    rands::RomuDuoJrRand,
    shmem::{MmapShMem, MmapShMemProvider, ShMemProvider, ShMemId},
    Named, HasLen, AsSliceMut,
};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::hash::{Hasher, Hash};

const FACTOR: f64 = 1337.0;

#[derive(Debug, Clone)]
pub struct UniformDistribution {}

impl<I, S> TestcaseScore<I, S> for UniformDistribution
where
    S: HasCorpus<I>,
{
    fn compute(_state: &S, _: &mut Testcase<I>) -> Result<f64, Error> {
        Ok(FACTOR)
    }
}

pub type UniformProbabilitySamplingScheduler =
    ProbabilitySamplingScheduler<UniformDistribution>;


#[derive(uniffi::Object, Debug)]
pub struct LibAflObject {
    state: Arc<Mutex<StdState<OnDiskCorpus<BytesInput>, BytesInput, RomuDuoJrRand, InMemoryCorpus<BytesInput>>>>,
    scheduler: Arc<Mutex<UniformProbabilitySamplingScheduler>>,
    _shmem: Arc<Mutex<MmapShMem>>, // Keep shared memory alive
}

unsafe impl Send for LibAflObject {}
unsafe impl Sync for LibAflObject {}

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


#[uniffi::export]
impl LibAflObject {
    #[uniffi::constructor]
    pub fn new(corpus_dir: String, shmem_key: String) -> Arc<Self> {
        let mut shmem_provider = MmapShMemProvider::new().expect("Failed to create shared memory provider");
        let shmem_id = ShMemId::from_string(&shmem_key);
        let shmem = shmem_provider
            .shmem_from_id_and_size(shmem_id, 0x200000)
            .expect("Failed to attach to shared memory");

        let shmem_arc = Arc::new(Mutex::new(shmem));

        let shared_mem_slice: &'static mut [u8] = {
            let mut shmem_locked = shmem_arc.lock().unwrap();
            unsafe { std::mem::transmute::<&mut [u8], &'static mut [u8]>(shmem_locked.as_slice_mut()) }
        };

        let coverage_data = &shared_mem_slice[4..];
        let num_edges = u32::from_le_bytes(shared_mem_slice[0..4].try_into().unwrap()) as usize;
         // Create a clone of the slice for accounting map creation
        let accounting_map: Vec<u32> = coverage_data
        .iter()
        .take(num_edges)
        .map(|&byte| byte as u32)
        .collect();

        let raw_observer = FuzzilliCoverageObserver::new("fuzzilli_coverage", shared_mem_slice);
        let observer = raw_observer.track_indices();

        let on_disk_corpus = OnDiskCorpus::<BytesInput>::new(&corpus_dir).expect("Failed to create OnDiskCorpus");
        let in_memory_corpus = InMemoryCorpus::<BytesInput>::new();

        let rng = RomuDuoJrRand::with_seed(12345);

        let mut feedback = MaxMapFeedback::new(&observer);
        let mut objective_feedback = MaxMapFeedback::new(&observer);

        let mut state = StdState::new(
            rng,
            on_disk_corpus,
            in_memory_corpus,
            &mut feedback,
            &mut objective_feedback,
        )
        .expect("Failed to initialize StdState");

        let mut scheduler: ProbabilitySamplingScheduler<_> = UniformProbabilitySamplingScheduler::new();
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
        state.corpus_mut().add(testcase).expect("Failed to add testcase to corpus");
    }


    pub fn suggest_next_input(&self) -> Vec<u8> {
        let mut scheduler = self.scheduler.lock().unwrap();
        let mut state = self.state.lock().unwrap();
        let next_id = scheduler.next(&mut *state).expect("Failed to fetch next input ID");
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
