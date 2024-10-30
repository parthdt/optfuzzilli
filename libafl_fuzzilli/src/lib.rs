#![allow(unused_imports, dead_code, unused_variables)]
#[cfg(windows)]
use alloc::{borrow::ToOwned, rc::Rc, string::String, vec::Vec};
use core::{
    cell::RefCell,
    hash::{BuildHasher, Hasher},
};
#[cfg(feature = "std")]
use std::{fs::File, io::Read, path::Path, path::PathBuf};
use std::sync::{Arc, Mutex};
use std::marker::PhantomData;
use ahash::RandomState;
#[cfg(feature = "std")]
use libafl_bolts::{fs::write_file_atomic, Error};
use libafl_bolts::{ownedref::OwnedSlice, HasLen};
use serde::{Deserialize, Serialize};
use libafl::monitors::SimpleMonitor;
use libafl::{
    corpus::{Corpus, CorpusId, InMemoryCorpus, Testcase, HasTestcase, OnDiskCorpus},
    events::SimpleEventManager,
    executors::{inprocess::InProcessExecutor, ExitKind},
    feedbacks::{CrashFeedback, ConstFeedback, MaxMapFeedback},
    fuzzer::{Fuzzer, StdFuzzer},
    generators::RandPrintablesGenerator,
    inputs::{BytesInput, HasTargetBytes, HasMutatorBytes, Input},
    mutators::scheduled::{havoc_mutations, StdScheduledMutator},
    observers::StdMapObserver,
    schedulers::{QueueScheduler, Scheduler},
    stages::mutational::StdMutationalStage,
    state::{StdState,HasCorpus,State, UsesState},
};

use libafl_bolts::{current_nanos, rands::StdRand, tuples::tuple_list, AsSlice};
use rand::Rng;

fn print_type_of<T>(_: &T) {
    println!("{}", std::any::type_name::<T>())
}


// Define your wrapper struct
#[derive(Default, Serialize, Deserialize, Clone, Debug)]
#[serde(bound = "I: serde::de::DeserializeOwned")]
pub struct FzilOnDiskCorpus<I>
where
    I: Input,
{
    inner: OnDiskCorpus<I>,
}

// Implement Send and Sync for FzilOnDiskCorpus
unsafe impl<I: Input + Send + Sync> Send for FzilOnDiskCorpus<I> {}
unsafe impl<I: Input + Send + Sync> Sync for FzilOnDiskCorpus<I> {}

// Concrete type for uniffi
#[derive(uniffi::Object, Debug)]
pub struct FzilOnDiskCorpusBytes {
    inner: Arc<Mutex<FzilOnDiskCorpus<BytesInput>>>,
}

// Implementation for FzilOnDiskCorpusBytes
#[uniffi::export]
impl FzilOnDiskCorpusBytes {
    #[uniffi::constructor]
    pub fn new() -> Arc<FzilOnDiskCorpusBytes> {
        let inner_corpus = FzilOnDiskCorpus {
            inner: OnDiskCorpus::new(PathBuf::from("./pcorpus")).unwrap(),
        };
        Arc::new(FzilOnDiskCorpusBytes {
            inner: Arc::new(Mutex::new(inner_corpus)),
        })
    }

    pub fn add_input(&self, input: Vec<u8>) {
        let input = BytesInput::new(input);
        let testcase = Testcase::new(input);

        // Lock the mutex to obtain a mutable reference to the inner corpus
        if let Ok(mut inner_corpus) = self.inner.lock() {
            // Now call add on the inner mutable reference
            inner_corpus.inner.add(testcase).unwrap();
        } else {
            // Handle the case where mutex lock fails
            println!("Unable to obtain mutable reference to inner corpus");
            // Optionally panic if locking fails
            // panic!("Mutex lock failed");
        }
    }

    pub fn count(&self) -> u64 {
        let inner_corpus = self.inner.lock().unwrap();
        let count = inner_corpus.inner.count();
        count as u64
    }

    pub fn ids(&self)
    {
        let inner_corpus = self.inner.lock().unwrap();

        let id = inner_corpus.inner.current().map(|id| inner_corpus.inner.next(id)).flatten().unwrap_or_else(|| inner_corpus.inner.first().unwrap());
        println!("Current ID: {}", id);
        println!("Last ID: {}", inner_corpus.inner.last().expect("Cant get last id"));
        println!("First ID: {}", inner_corpus.inner.first().expect("cant get first id"));
    }

  
    pub fn first_index(&self) -> u64{
        
        let inner_corpus = self.inner.lock().unwrap();
        let first_id = inner_corpus.inner.first().unwrap();

        let first_id_usize : usize = first_id.into();
        let first_id_u64 : u64 = first_id_usize as u64;
        first_id_u64
    }

    pub fn last_index(&self) -> u64{
        
        let inner_corpus = self.inner.lock().unwrap();
        let last_id = inner_corpus.inner.last().unwrap();

        let last_id_usize : usize = last_id.into();
        let last_id_u64 : u64 = last_id_usize as u64;
        last_id_u64
    }

    pub fn next_free(&self){
        let inner_corpus = self.inner.lock().unwrap();

        println!("{}", inner_corpus.inner.peek_free_id());
    }

    pub fn get_element(&self, corpus_id: u64) -> Vec<u8> {
        let inner_corpus = self.inner.lock().unwrap();
        let corpus_id = CorpusId::from(corpus_id as usize);
        match inner_corpus.inner.get(corpus_id) {
            Ok(testcase) => {
                if let Some(input) = testcase.borrow().input() {
                    input.bytes().to_vec()
                } else {
                    Vec::new() // Return an empty Vec<u8> if the input is None
                }
            }
            Err(_) => Vec::new(), // Return an empty Vec<u8> if the corpus_id is invalid
        }
    }
    
    pub fn get_random_element(&self) -> Vec<u8> {
        let first_index = self.first_index();
        let last_index = self.last_index();
        
        if first_index > last_index {
            return Vec::new(); // Return an empty Vec<u8> if the range is invalid
        }
        
        let mut rng = rand::thread_rng();
        let random_index = rng.gen_range(first_index..=last_index);
        
        self.get_element(random_index)
    }
    
}

#[derive(uniffi::Object, Debug)]
pub struct MyFzilScheduler {
    inner: Arc<Mutex<QueueScheduler<StdState<BytesInput, OnDiskCorpus<BytesInput>, StdRand, OnDiskCorpus<BytesInput>>>>>,
    state: Arc<Mutex<StdState<BytesInput, OnDiskCorpus<BytesInput>, StdRand, OnDiskCorpus<BytesInput>>>>,
}

unsafe impl Send for MyFzilScheduler {}
unsafe impl Sync for MyFzilScheduler {}

#[uniffi::export]
impl MyFzilScheduler {
    // Constructor to create a new QueueScheduler with StdState
    #[uniffi::constructor]
    pub fn new() -> Arc<MyFzilScheduler> {
        let rand = StdRand::with_seed(current_nanos());
        let corpus1 = OnDiskCorpus::new(PathBuf::from("./pcorpus")).unwrap();
        let corpus2 = OnDiskCorpus::new(PathBuf::from("./ocorpus")).unwrap();
        
        let state = StdState::new(
            rand,
            corpus1,
            corpus2,
            &mut ConstFeedback::new(false),
            &mut ConstFeedback::new(false),
        ).unwrap();

        let scheduler = QueueScheduler::new();

        Arc::new(MyFzilScheduler {
            inner: Arc::new(Mutex::new(scheduler)),
            state: Arc::new(Mutex::new(state)),
        })
    }

    // Add an input to the corpus
    pub fn add_input(&self, input_data: Vec<u8>) {
        let input = BytesInput::new(input_data);
        let testcase = Testcase::new(input);
        let mut state = self.state.lock().unwrap();
        state.corpus_mut().add(testcase).unwrap();
    }

    // Get the current test case in the scheduler, returns Vec<u8>
    pub fn current_testcase(&self) -> Vec<u8> {
        let state = self.state.lock().unwrap();
        let current_id = state.corpus().current().unwrap();

        // Retrieve the testcase from the corpus using current_id
        let testcase = state.corpus().get(current_id).unwrap();
        let testcase_borrowed = testcase.borrow();  // Borrow the testcase
        let input = testcase_borrowed.input().as_ref().unwrap();
        input.bytes().to_vec()  // Return as Vec<u8>
    }

    // Get the next input from the scheduler, returns Vec<u8>
    pub fn next_input(&self) -> Vec<u8> {
        let mut scheduler = self.inner.lock().unwrap();
        let mut state = self.state.lock().unwrap();
        let next_id = scheduler.next(&mut *state).unwrap();

        let testcase = state.corpus().get(next_id).unwrap();  // Get the testcase
        let testcase_borrowed = testcase.borrow();  // Borrow the testcase
        let input = testcase_borrowed.input().as_ref().unwrap();
        input.bytes().to_vec()  // Return as Vec<u8>
    }
}

uniffi::setup_scaffolding!();
