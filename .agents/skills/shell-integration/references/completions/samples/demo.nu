module completions {
    # Do the sub thing
    export extern demo [
        --help(-h) # Print help
    ]

    # Do the sub thing
    export extern "demo sub" []

    # Print help
    export extern "demo help" []
}

export use completions *
