# wayland compositor

i am not even gonna try and name this. not my best project or even good it was just a sort of practice for me to "TRY" and learn/make a wayland compositor

# build
```
cargo build
```

# what is this?
this is hell. okay yeah this is supposed to be a fun project so i learn wayland and how my hyprdots work

The goal of this project was not to make a production-ready compositor.

# technologies
- rust
- wayland
- smithay
- DRM/KMS
- Linux graphics stack

# note
if youre here to use this as a reference or anything

please leave.

and also if youre trying to run this using the release or using cargo run

1) go to a new tty using ctrl + alt + f2 or f3 or wtv tty
2) do cargo run or run the executable


# technical rant
we are first setting up a `Display` with the capabilities of this compositor to then accept clients

to accept those client we have to advertise our capabilities to them 

then they connect and we then create a connection with the display we set up earlier.

the client then tries to use the xdg-shell protocol to try and request a window but BEFORE THAT

the client then creates a surface however many they want EACH client and then and only THEN

after giving them this surface the client tries to map a window on this surface but hold on

mapping a window would be too simple no no no

the client tries to draw a buffer and attaches it to a surface and commits it??? committing???

and then the compositor accepts it. manages the buffer sends it over to the drm oh did i forget we are actually configuring hardware ourselves. yeah the drm + kms. ig vro so we send it over to drm to render

we have to keep track of every step here in memory plus have shared memory across threads so that adds the added bonus of deadlocks and oh my god kill me

we then render the framebuffer and send it to client

# ai use
i was trying to go of by the docs but at one point i for frustrated and started vibe coding. i mean still only for help not entire project. i did write a chunk of this by hand

