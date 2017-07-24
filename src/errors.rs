error_chain!{
    foreign_links {
        Io(::std::io::Error);
        Uri(::hyper::error::UriError);
        Json(::serde_json::Error);
        Hyper(::hyper::Error);
    }

    errors {
        TooManyRequests(try_again_at: usize) {
            description("too many requests in window")
            display("too many requests in window, try again at {}", try_again_at)
        }
    }
}
