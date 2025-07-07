# GDB initialization script for debugging the kernel
set confirm off
set architecture aarch64
set print pretty on

# Connect to QEMU's GDB server
target remote localhost:1234

# Set breakpoints at key points
break _start

layout asm