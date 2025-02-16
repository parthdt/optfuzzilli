#![allow(unused_imports, dead_code, unused_variables)]

use std::sync::{Arc, Mutex};
use libafl::{
    corpus::{InMemoryCorpus, OnDiskCorpus, Testcase, Corpus, CorpusId},
    feedbacks::{MaxMapFeedback, MapFeedback},
    inputs::{BytesInput, HasMutatorBytes},
    observers::{CanTrack, MapObserver, ExplicitTracking},
    schedulers::{ProbabilitySamplingScheduler, TestcaseScore, Scheduler},
    state::{StdState, HasCorpus},
    Error, HasMetadata, HasNamedMetadata
};
use libafl_bolts::{
    rands::RomuDuoJrRand,
    shmem::{MmapShMem, MmapShMemProvider, ShMemProvider, ShMemId},
    Named, HasLen, AsSliceMut, AsSlice,
};
use libafl_bolts::impl_serdeany;
use libafl_bolts::serdeany::SerdeAny;
use std::any::Any;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

const FACTOR: f64 = 1337.0;

/// **Uniform Probability Distribution for Sampling Scheduler**
#[derive(Debug, Clone)]
pub struct UniformDistribution {}

impl<S> TestcaseScore<BytesInput, S> for UniformDistribution
where
    S: HasCorpus<BytesInput> + HasMetadata + HasNamedMetadata,
{
    fn compute(state: &S, testcase: &mut Testcase<BytesInput>) -> Result<f64, Error> {
        // Fetch observer from metadata
        let observer = state
            .metadata_map()
            .get::<FuzzilliCoverageObserver>()
            .ok_or_else(|| Error::key_not_found("FuzzilliCoverageObserver not found"))?;

        let coverage_count = observer.count_bytes() as f64;
        let input_length = testcase.input().as_ref().map(|i| i.len() as f64).unwrap_or(0.0);
        
        // Score based on coverage count and input length
        let score = (coverage_count * 10.0) - (input_length * 0.1);
        Ok(score.max(1.0)) // Ensure min score of 1.0
    }
}

pub type UniformProbabilitySamplingScheduler =
    ProbabilitySamplingScheduler<UniformDistribution>;

/// **LibAFL Wrapper Object for Fuzzilli**
#[derive(uniffi::Object, Debug)]
pub struct LibAflObject {
    state: Arc<Mutex<StdState<OnDiskCorpus<BytesInput>, BytesInput, RomuDuoJrRand, InMemoryCorpus<BytesInput>>>>,
    scheduler: Arc<Mutex<UniformProbabilitySamplingScheduler>>,
    _shmem: Arc<Mutex<MmapShMem>>, // Keep shared memory alive
}

unsafe impl Send for LibAflObject {}
unsafe impl Sync for LibAflObject {}

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
        self.map.clone()
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

        let shared_mem_vec = {
            let shmem_locked = shmem_arc.lock().unwrap();
            shmem_locked.as_slice().to_vec()
        };

        let observer = FuzzilliCoverageObserver::new("fuzzilli_coverage", shared_mem_vec);

        let mut state = StdState::new(
            RomuDuoJrRand::with_seed(12345),
            OnDiskCorpus::new(&corpus_dir).expect("Failed to create corpus"),
            InMemoryCorpus::new(),
            &mut MaxMapFeedback::new(&observer),
            &mut MaxMapFeedback::new(&observer),
        ).expect("Failed to initialize state");

        // Store observer in metadata
        state.metadata_map_mut().insert(observer);

        let mut scheduler = UniformProbabilitySamplingScheduler::new();

        Arc::new(Self {
            state: Arc::new(Mutex::new(state)),
            scheduler: Arc::new(Mutex::new(scheduler)),
            _shmem: shmem_arc,
        })
    }

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
        input.mutator_bytes().to_vec()
    }

    pub fn count(&self) -> u64 {
        let state = self.state.lock().unwrap();
        state.corpus().count() as u64
    }

    pub fn first_index(&self) -> u64 {
        let state = self.state.lock().unwrap();
        state.corpus().first().map(|id| id.into()).unwrap_or(0) as u64
    }

    pub fn last_index(&self) -> u64 {
        let state = self.state.lock().unwrap();
        state.corpus().last().map(|id| id.into()).unwrap_or(0) as u64
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
