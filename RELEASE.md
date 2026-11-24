Unpack the zip anywhere and add the folder in the Windows Toolbox with **Add program…** – or get it there with
one click under **Get programs**.

The `.exe` is not signed (signing costs money). Windows SmartScreen may therefore warn once at the first start of a
downloaded program: *More info → Run anyway*. Updates that the Toolbox downloads are checked against the program's
own signature and do not ask.

After adding it, press **Set up** on its page once: it installs the driver PawnIO, LibreHardwareMonitor and a small
bridge into a folder only admins can write to, and a task in Task Scheduler – Windows asks for admin rights once.
The monitor itself runs without admin rights.

**Version 3** is written in Rust and needs no Python any more. Fan curves, the detail window's place and the power
logs of version 2 are taken over at the first start. Coming from version 2: press **Remove setup** there before
updating – it removes the old task "HardwareMonitor" (keep PawnIO).
