# Task: Developer Questions for unified-hifi-control

The repo is at /Users/muness1/src/hiphi-repos/unified-hifi-control. A Rust hi-fi control bridge with adapters for Roon, LMS, OpenHome, UPnP, and HQPlayer.

Answer these 5 questions as if you're a developer about to make changes. Be specific — name files, functions, line numbers.

## Q1: "I need to add a volume step size override per zone. What code path do I need to understand?"

Trace the full volume change flow from when a user clicks volume-up in the web UI to when the audio device changes volume. Name every function and file in the chain. I need to know where to add the override logic.

## Q2: "I'm adding a new adapter. What's the minimal set of things I need to implement?"

List every trait, struct, macro, and file I need to touch to add a new adapter that follows the existing patterns. Show me the specific interfaces with their method signatures.

## Q3: "The OpenHome adapter's SOAP parsing is buggy. What tests cover it, and what's untested?"

Tell me which functions in the OpenHome adapter have test coverage (direct or transitive through integration tests) and which don't. I need to know where to add tests before I refactor.

## Q4: "I want to refactor the Zone struct. What breaks?"

The Zone struct in bus/events.rs is getting too big. If I split it or change its fields, what code would I need to update? Give me the full list of files and functions that construct, destructure, or pass Zone values.

## Q5: "Which adapter has the most complex control flow? I need to assess tech debt."

Rank the adapters by code complexity. For the most complex one, show me the highest-complexity functions and what they call. I'm deciding where to invest refactoring effort.
