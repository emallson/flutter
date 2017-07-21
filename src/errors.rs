error_chain!{
    foreign_links {
        Io(::std::io::Error);
        Uri(::hyper::error::UriError);
        Json(::serde_json::Error);
        Hyper(::hyper::Error);
    }
}
