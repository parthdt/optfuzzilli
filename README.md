## Optmising Fuzzilli

Layout:

- fuzzilli: contains a fork of Fuzzilli that would be modified to work with the exported LibAFL structs and features. See `LIBAFL.md` for more details.

- libafl_fuzzilli: library in rust using features from LibAFL, wrapped with macros from uniffi to be exported to swift. The shared library will be used to create a swiftmodule, and imported in fuzzilli. Please see the corresponding README for steps to build and test. 

