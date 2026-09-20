MEMORY
{
    /* Reserve the final 256 KiB (64 erase sectors) for queued measurements. */
    FLASH   : ORIGIN = 0x10000000, LENGTH = 3840K
    STORAGE : ORIGIN = 0x103c0000, LENGTH = 256K
    RAM     : ORIGIN = 0x20000000, LENGTH = 512K
}

__storage_start = ORIGIN(STORAGE);
__storage_end = ORIGIN(STORAGE) + LENGTH(STORAGE);

ASSERT(ORIGIN(STORAGE) == ORIGIN(FLASH) + LENGTH(FLASH),
       "storage must immediately follow firmware flash");
ASSERT((ORIGIN(STORAGE) % 4096) == 0,
       "storage start must be erase-aligned");
ASSERT((LENGTH(STORAGE) % 4096) == 0,
       "storage length must be erase-aligned");
ASSERT(__storage_end == 0x10400000,
       "storage must end at the end of the 4 MiB flash");

SECTIONS {
    .start_block : ALIGN(4)
    {
        __start_block_addr = .;
        KEEP(*(.start_block));
        KEEP(*(.boot_info));
    } > FLASH
} INSERT AFTER .vector_table;

_stext = ADDR(.start_block) + SIZEOF(.start_block);

SECTIONS {
    .bi_entries : ALIGN(4)
    {
        __bi_entries_start = .;
        KEEP(*(.bi_entries));
        . = ALIGN(4);
        __bi_entries_end = .;
    } > FLASH
} INSERT AFTER .text;

SECTIONS {
    .end_block : ALIGN(4)
    {
        __end_block_addr = .;
        KEEP(*(.end_block));
        __flash_binary_end = .;
    } > FLASH
} INSERT AFTER .uninit;

PROVIDE(start_to_end = __end_block_addr - __start_block_addr);
PROVIDE(end_to_start = __start_block_addr - __end_block_addr);

ASSERT(__flash_binary_end <= __storage_start,
       "firmware overlaps reserved measurement storage");
