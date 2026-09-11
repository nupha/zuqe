#!/usr/sbin/dtrace -s

#pragma D option quiet

profile-997
/pid == $target && probefunc != ""/
{
    @function_count[probefunc] = count();
}

proc:::exit
/pid == $target/
{
    printf("\n=== Function Profile Results ===\n");
    printf("%-60s %10s\n", "FUNCTION", "SAMPLES");
    printf("%-60s %10s\n", "--------", "-------");
    printa("%-60s %@10d\n", @function_count);
    exit(0);
}

