## LibAFL - Fuzzilli

This directory contains code used to export features from LibAFL to Fuzzilli.

- `src/` directory contains the library. It contains wrappers for LibAFL features, along with an API to use them on the swift side.
These are then exported to swift using uniffi macros.

To build locally, do:

1. `cargo build`. This will build a shared library in the `target/debug` folder. Also, the `uniffi-bindgen` binary will be built. This is used to create the bindings for swift.
2. `cargo run --bin uniffi-bindgen generate --library target/debug/liblibafl_fuzzilli.so --language swift --out-dir out`. This will use the shared library built and create swift bindings which can be used to create the swift module.

Your `out` directory would look something like:
```
out/
├── libafl_fuzzilliFFI.h
├── libafl_fuzzilliFFI.modulemap
└── libafl_fuzzilli.swift
```

3. Navigate to the `out` directory.
4. Copy the shared library from the `target/debug` folder. So: `cp ../target/debug/liblibafl_fuzzilli.so .` This is required for buiding the swiftmodule.
5. Build the swiftmodule by running: 
```
swiftc -emit-module -module-name fs -o libfs.so -emit-library -Xcc -fmodule-map-file=`pwd`/libafl_fuzzilliFFI.modulemap -I . -L . -llibafl_fuzzilli libafl_fuzzilli.swift
```
This would build the `fs.swiftmodule`.

6. Test code from the module by creating a file, and running it using swift. An example file, `test.swift` is provided in the repo. Copy it to the `out` directory, and run: 
```
swift -I . -L . -lfs -Xcc -fmodule-map-file=`pwd`/libafl_fuzzilliFFI.modulemap test.swift
```