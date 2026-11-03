PRIM — Windows test build (identity round 1)
=============================================

A private test build for a few friends. It is not the public release: it does not
auto-update and only works with other people on this same build ("hello v6").
Please don't share the zip.

1. INSTALL
   - Unzip the whole folder somewhere ordinary (for example C:\Prim or your Desktop).
     Don't run it from inside the zip file, and avoid "Program Files".
   - If Windows says a DLL is missing (e.g. VCRUNTIME140.dll), run VC_redist.x64.exe
     from this folder once, then try again.

2. START
   - Double-click Desktop.bat to run on the flat screen (no headset needed).
     Play.bat starts VR if a SteamVR/OpenXR headset is running, else desktop.
     (Running prim.exe directly also works.)
   - First run: Windows SmartScreen may say "Windows protected your PC" because the
     file is unsigned. Click "More info" -> "Run anyway".
   - First run: Windows Firewall asks whether to allow prim.exe. Tick PRIVATE
     networks AND PUBLIC networks, then Allow. Prim needs it to connect to friends.
     (If you clicked Cancel by mistake: Windows Security -> Firewall -> "Allow an
     app through firewall" -> prim.exe.)
   - Voice: allow the microphone if Windows asks (Settings -> Privacy -> Microphone ->
     let desktop apps use it). Use headphones, or you will hear yourself.

3. EXCHANGE FRIEND CODES (once, with S)
   - In Prim open Menu -> Friends. Your friend code is at the top; press Copy.
   - Send your code to S in Discord/chat, and paste S's code into the box,
     type a name for them, and press Add. (S does the same with yours.
     Both sides must add each other.)
   - Quit Prim completely, and start it again. The Friends tab should still list
     S under the name you gave.
   - Your code lives in %APPDATA%\prim\identity.key. Don't delete that file; it is
     who you are in Prim. (Deleting it gives you a new code and S must re-add you.)

4. JOIN S'S ROOM
   - S chooses Room -> Host Friends instance. Then you open Menu -> Friends and
     press Join next to S's name.
   - You should see S's avatar and hear S. Unmute yourself in the menu to talk.
   - If it sits on "Joining..." for more than ~15 seconds it says "host did not
     answer": S may not be hosting yet, or the network is blocking it. Try again after
     a minute. If it says "not admitted", S hasn't added your code (or it is the
     wrong code).

5. IF SOMETHING GOES WRONG
   - Tell S what you pressed and what you saw. Logs are in %APPDATA%\prim\logs
     (send the newest file). Screenshots help.
