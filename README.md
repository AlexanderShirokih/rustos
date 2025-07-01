# Kernel Debugging Guide

This project is a simple bare-metal kernel for aarch64 architecture that can be run and debugged using QEMU and GDB.

## Building the Kernel

To build the kernel, run:

```bash
cargo build
```

Or use the Makefile.toml task:

```bash
cargo make build
```

## Running the Kernel

To run the kernel in QEMU, use:

```bash
cargo make qemu
```

This will start QEMU with the kernel loaded and run it in the terminal.

## Debugging the Kernel

To debug the kernel, use:

```bash
cargo make debug
```

This will:
1. Start QEMU in the background with debugging enabled
2. Launch GDB connected to QEMU
3. Load the .gdbinit script with helpful debugging configurations

### GDB Commands

Once in GDB, you can use the following commands:

- `c` or `continue`: Continue execution
- `s` or `step`: Step into the next instruction
- `n` or `next`: Step over the next instruction
- `examine_hello`: View the "Hello, world!" message in memory (custom command)
- `info registers`: Show all registers
- `x/10i $pc`: Show the next 10 instructions from the current program counter

### Debugging Process

1. When you run `cargo make debug`, GDB will stop at the `_start` function
2. Use `n` to step through the code line by line
3. After the message is written to memory, use `examine_hello` to view it
4. Use `c` to continue execution (the program will enter an infinite loop)
5. Press Ctrl+C to interrupt execution and return to GDB
6. Use `quit` to exit GDB (QEMU will be automatically terminated)

## Code Explanation

The kernel does the following:

1. Starts execution at the `_start` function
2. Writes "Hello, world!" to memory address 0x4010_1000
3. Enters an infinite loop with the `wfe` (Wait For Event) instruction

The memory address 0x4010_1000 was chosen to be within the memory region defined in the linker script (starting at 0x40100000).