/* Static memory budget, checked at link time (structural, not proven):
   statics (.data + .bss, which hold the executor's task and so the
   connection's buffers) plus a 16 KiB stack must fit in the 128 KiB of RAM. */
_stack_reserve = 16K;
ASSERT(SIZEOF(.data) + SIZEOF(.bss) + SIZEOF(.uninit) + _stack_reserve <= LENGTH(RAM),
       "thermostat-firmware: statics + stack reserve exceed RAM");
