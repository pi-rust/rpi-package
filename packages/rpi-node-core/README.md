# Shared node owner

This package extracts Work's existing multi-project owner and native/ACP runtime
adapters. HTTP and private local control receive the same owner instance; native
transcripts remain authoritative and UI projections stay in Work.

The distributable crate pins the published `rpi-extcore =0.2.7`. Work's original
Git revision `17ba89c` carried unpublished additions under a `0.2.6` version;
it is not interchangeable with crates.io `0.2.6`. Publishing that Git dependency
as `0.2.6` would silently change downstream source. The nearest published version
was selected explicitly and qualified with the complete node package tests and
actual HTTP extension ABI startup/configuration/cleanup tests. This is a runtime
dependency change from the original Git pin, not merely a lockfile rewrite.

CI selects workspace crates by their full Cargo package IDs because the legacy
node dependency and current workspace protocol adapters can have the same names.
