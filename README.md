bofh
====

This is a Rust implementation of `bofh` (_BrukerOrganisering For Hvermansen_), a client for the [Cerebrum](https://github.com/unioslo/cerebrum) IGA (Identity Governance and Administration) system.

Features
--------

* Tab completion
* Command and argument hints as you type (with colors)
* Persistent history
* Emacs/Bash or Vi-style editing mode

Library
-------

A Rust library is provided, although it's less ergonomical than its Python counterpart ([pybofh](https://pypi.org/project/bofh/)), and is mostly a thin wrapper. In particular, it doesn't keep track of the bofhd server's supported commands; it's expected that the implementing client does this.

See also
--------

* [jbofh](https://github.com/unioslo/jbofh) (Java)
* [pybofh](https://github.com/unioslo/pybofh) (Python)
* wofh/[Brukerinfo](https://github.com/unioslo/brukerinfo) (PHP)
