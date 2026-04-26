/* Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved. */
ENTRY(_start)

PHDRS
{
  text PT_LOAD FLAGS(5);
  data PT_LOAD FLAGS(6);
}

SECTIONS
{
  /* Link protected services at the real per-process user slot base so the
     kernel can map them directly without applying ELF relocations. */
  . = 0x0000008000400000;

  .text : ALIGN(0x1000)
  {
    *(.text .text.*)
    *(.ltext .ltext.*)
  } :text

  .rodata : ALIGN(0x1000)
  {
    *(.rodata .rodata.*)
    *(.lrodata .lrodata.*)
  } :text

  .data : ALIGN(0x1000)
  {
    *(.data .data.*)
  } :data

  .bss : ALIGN(0x1000)
  {
    *(.bss .bss.* COMMON)
  } :data

  /DISCARD/ :
  {
    *(.eh_frame*)
    *(.comment*)
    *(.note*)
  }
}
