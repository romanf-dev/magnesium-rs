/*
 * Linker script for RP2350.
 * Scratch X/Y are used as stacks for cores 0/1 respectively.
 * This script uses 'fake' FLASH at the beginning of the RAM to keep 
 * whole image in RAM (data copying from 'flash' to ram is also kept).
 * Adjust flash address to actual flash to make it persistent.
 */

MEMORY
{
    FLASH (rx) : ORIGIN = 0x20000000, LENGTH = 64k
    RAM(rwx) : ORIGIN =  0x20010000, LENGTH = 448k
    SCRATCH_X(rwx) : ORIGIN = 0x20080000, LENGTH = 4k
    SCRATCH_Y(rwx) : ORIGIN = 0x20081000, LENGTH = 4k
}

ENTRY(elf_entry)
INPUT(startup.o)

_estack0 = ORIGIN(SCRATCH_X) + LENGTH(SCRATCH_X);
_estack1 = ORIGIN(SCRATCH_Y) + LENGTH(SCRATCH_Y);

SECTIONS
{
    .text :
    {
        PROVIDE(_vector_table = ABSOLUTE(.));
        KEEP(*(.vectors))
        KEEP(*(.metadata_block))
        KEEP(*(.exceptions))
        *(.text*)
        *(.rodata*)
    } > FLASH

    _sidata = LOADADDR(.data);

    .data :
    {
        _sdata = .;
        *(.data*)
        *(.sdata*)
        *(.tdata*)
        _edata = .;
    } > RAM AT> FLASH

    .bss :
    {
        _sbss = .;
        *(.bss*)
        *(.sbss*)
        *(COMMON)
        _ebss = .;
    } > RAM

    /DISCARD/ :
    {
        *(.ARM.exidx*)
        *(.debug*)
    }
}

