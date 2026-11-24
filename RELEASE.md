Unpack the zip anywhere and add the folder in the Windows Toolbox with **Add program…** – or get it there with
one click under **Get programs**.

The `.exe` is not signed (signing costs money). Windows SmartScreen may therefore warn once at the first start of a
downloaded program: *More info → Run anyway*. Updates that the Toolbox downloads are checked against the program's
own signature and do not ask.

After adding it, press **Set up** on its page once: it installs the driver PawnIO, LibreHardwareMonitor and a small
bridge into a folder only admins can write to, and a task in Task Scheduler – Windows asks for admin rights once.
The monitor itself runs without admin rights.

## New in 1.0.1

- **Fans tab:** a switch Graphics card | Motherboard | CPU. Motherboard headers can be given a name (it shows in
  the other tabs too) and be marked as CPU fan; headers that do not turn fold away. "Boost" runs a header at 100 %
  for 5 seconds, so you hear which fan it is. Ctrl / Shift+click selects several fans and sets them together. The
  graphics card shows its fan channels (the driver reports channels, not single fans).
- **Power tab:** stacked bars – CPU + graphics card as their sensors measure them, everything else the smart plug
  measures, the rest of the PC estimated where there is no plug. A new view "60 minutes" (per minute, in W), the
  running period with what is still expected, an average line, kWh or money, a line of the last 2 minutes in
  "Now" and a forecast for the month. On the Toolbox page: the chart as bars or lines.
- **The detail window** has a quieter, more compact look; the bottom line keeps room for two lines.
- **The power logs start anew:** they now keep CPU + GPU per hour; logs of 1.0.0 are discarded at the first start.

Written in Rust, without Python.
