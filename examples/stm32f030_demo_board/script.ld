
MEMORY
{
     FLASH (rx) : ORIGIN = 0x08000000, LENGTH = 16K
     SRAM (rwx) : ORIGIN = 0x20000000, LENGTH = 4K
}

ENTRY(RESET_Handler)

INPUT(startup.o)


SECTIONS
{
	.text :
	{
	    KEEP(*(.vectors*))
		*(.text*)
		*(.rodata*)
		_etext = .;
	} > FLASH
    
    /DISCARD/ :
    {
        *(.ARM.exidx*)
    }

	.data : AT (_etext)
	{
		_data = .;
		*(vtable)
		*(.data*)
		_edata = .;
	} > SRAM

	/* zero initialized data */
	.bss :
	{
		_bss = .;
		*(.bss*)
		*(COMMON)
		_ebss = .;
	} > SRAM

    /DISCARD/ :
    {
        *(.debug*)
    }

	_data_sz   = (_edata - _data);
	_bss_sz   = _ebss - _bss;
}

