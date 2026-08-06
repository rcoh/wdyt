//! Module-derived HTTP route tree.

use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::{Router, layout, page};
use topcoat::view::view;

use super::State;

pub(super) fn router(state: State) -> Router {
    topcoat::router::module_router!().app_context(state).build()
}

#[layout]
async fn root_layout(cx: &Cx, slot: Result) -> Result {
    let child = slot?;
    let (page, colors) = super::page_shell(cx)?;
    view! {
        super::shell(
            page: page,
            colors: &colors,
            child: child,
        )
    }
}

#[page]
async fn index(cx: &Cx) -> Result {
    super::index_page(cx).await
}

mod health {
    use topcoat::Result;
    use topcoat::context::Cx;
    use topcoat::router::{content::Json, route};

    #[route(GET)]
    async fn health(cx: &Cx) -> Result<Json<serde_json::Value>> {
        super::super::health_route(cx).await
    }
}

mod assets {
    mod wdyt_css {
        use topcoat::Result;
        use topcoat::router::{Response, route, segment};

        segment!(rename = "wdyt.css");

        #[route(GET)]
        async fn stylesheet() -> Result<Response> {
            super::super::super::stylesheet_route()
        }
    }
}

mod api {
    mod inbox {
        use topcoat::Result;
        use topcoat::context::Cx;
        use topcoat::router::{content::Json, route};

        #[route(GET)]
        async fn inbox(cx: &Cx) -> Result<Json<crate::server::Inbox>> {
            super::super::super::inbox_route(cx).await
        }
    }

    mod sessions {
        use topcoat::Result;
        use topcoat::context::Cx;
        use topcoat::router::{content::Json, route};

        #[route(POST)]
        async fn create(
            cx: &Cx,
            Json(input): Json<crate::server::CreateSession>,
        ) -> Result<Json<crate::server::CreatedSession>> {
            super::super::super::create_route(cx, Json(input)).await
        }

        mod session_id {
            use topcoat::router::segment;

            segment!(kind = Param, rename = "session_id");

            mod reply {
                use topcoat::Result;
                use topcoat::context::Cx;
                use topcoat::router::{content::Json, route};

                #[route(GET)]
                async fn reply(cx: &Cx) -> Result<Json<crate::server::AwaitedReply>> {
                    crate::server::await_reply_route(cx).await
                }
            }

            mod collect {
                use topcoat::Result;
                use topcoat::context::Cx;
                use topcoat::router::{content::Json, route};

                #[route(POST)]
                async fn collect(cx: &Cx) -> Result<Json<crate::server::AwaitedReply>> {
                    crate::server::collect_route(cx).await
                }
            }

            mod ack {
                use topcoat::Result;
                use topcoat::context::Cx;
                use topcoat::router::{content::Json, route};

                #[route(POST)]
                async fn ack(
                    cx: &Cx,
                    Json(input): Json<crate::server::AckInput>,
                ) -> Result<Json<crate::server::Acked>> {
                    crate::server::ack_route(cx, Json(input)).await
                }
            }

            mod status {
                use topcoat::Result;
                use topcoat::context::Cx;
                use topcoat::router::{content::Json, route};

                #[route(GET)]
                async fn status(cx: &Cx) -> Result<Json<crate::server::ReplyStatus>> {
                    crate::server::status_route(cx).await
                }
            }

            mod comments {
                use topcoat::Result;
                use topcoat::context::Cx;
                use topcoat::router::{content::Json, route};

                #[route(GET)]
                async fn comments(cx: &Cx) -> Result<Json<crate::server::Comments>> {
                    crate::server::list_comments_route(cx).await
                }

                mod comment_id {
                    use topcoat::router::segment;

                    segment!(kind = Param, rename = "comment_id");

                    mod messages {
                        use topcoat::Result;
                        use topcoat::context::Cx;
                        use topcoat::router::{content::Json, route};

                        #[route(POST)]
                        async fn messages(
                            cx: &Cx,
                            Json(input): Json<crate::server::MessageInput>,
                        ) -> Result<Json<crate::session::Message>> {
                            crate::server::agent_message_route(cx, Json(input)).await
                        }
                    }
                }
            }

            mod threads {
                use topcoat::Result;
                use topcoat::context::Cx;
                use topcoat::router::{content::Json, route};

                #[route(GET)]
                async fn threads(cx: &Cx) -> Result<Json<crate::server::Threads>> {
                    crate::server::agent_threads_route(cx).await
                }
            }

            mod recv {
                use topcoat::Result;
                use topcoat::context::Cx;
                use topcoat::router::{content::Json, route};

                #[route(GET)]
                async fn recv(cx: &Cx) -> Result<Json<crate::server::Received>> {
                    crate::server::recv_route(cx).await
                }
            }
        }
    }
}

pub(in crate::server) mod s {
    pub(in crate::server) mod session_id {
        use topcoat::Result;
        use topcoat::context::Cx;
        use topcoat::router::{page, path_param};

        #[path_param(error = not_found)]
        pub(in crate::server) struct SessionId(String);

        #[page]
        async fn session(cx: &Cx) -> Result {
            crate::server::session_page(cx).await
        }

        mod context {
            use topcoat::Result;
            use topcoat::context::Cx;
            use topcoat::router::{content::Json, route};

            #[route(GET)]
            async fn context(cx: &Cx) -> Result<Json<crate::server::ContextLines>> {
                crate::server::context_route(cx).await
            }
        }

        pub(in crate::server) mod comments {
            use topcoat::Result;
            use topcoat::context::Cx;
            use topcoat::router::{content::Json, route};

            #[route(POST)]
            async fn comments(
                cx: &Cx,
                Json(input): Json<crate::server::CommentInput>,
            ) -> Result<Json<crate::session::Comment>> {
                crate::server::comment_route(cx, Json(input)).await
            }

            pub(in crate::server) mod comment_id {
                use topcoat::router::path_param;

                #[path_param(error = not_found)]
                pub(in crate::server) struct CommentId(u64);

                mod messages {
                    use topcoat::Result;
                    use topcoat::context::Cx;
                    use topcoat::router::{content::Json, route};

                    #[route(POST)]
                    async fn messages(
                        cx: &Cx,
                        Json(input): Json<crate::server::MessageInput>,
                    ) -> Result<Json<crate::session::Message>> {
                        crate::server::thread_message_route(cx, Json(input)).await
                    }
                }
            }
        }

        mod threads {
            use topcoat::Result;
            use topcoat::context::Cx;
            use topcoat::router::{content::Json, route};

            #[route(GET)]
            async fn threads(cx: &Cx) -> Result<Json<crate::server::Threads>> {
                crate::server::threads_route(cx).await
            }
        }

        mod reply {
            use topcoat::Result;
            use topcoat::context::Cx;
            use topcoat::router::{content::Json, route};

            #[route(POST)]
            async fn reply(
                cx: &Cx,
                Json(input): Json<crate::server::ReplyInput>,
            ) -> Result<Json<crate::server::ReplyOutput>> {
                crate::server::reply_route(cx, Json(input)).await
            }
        }

        mod archive {
            use topcoat::Result;
            use topcoat::context::Cx;
            use topcoat::router::{Response, route};

            #[route(POST)]
            async fn archive(cx: &Cx) -> Result<Response> {
                crate::server::archive_route(cx).await
            }
        }

        mod restore {
            use topcoat::Result;
            use topcoat::context::Cx;
            use topcoat::router::{Response, route};

            #[route(POST)]
            async fn restore(cx: &Cx) -> Result<Response> {
                crate::server::restore_route(cx).await
            }
        }

        mod raw {
            use topcoat::Result;
            use topcoat::context::Cx;
            use topcoat::router::{Response, route};

            #[route(GET)]
            async fn raw(cx: &Cx) -> Result<Response> {
                crate::server::raw_route(cx).await
            }
        }

        mod assets {
            mod asset_path {
                use topcoat::Result;
                use topcoat::context::Cx;
                use topcoat::router::{Response, route, segment};

                segment!(kind = CatchAll, rename = "asset_path");

                #[route(GET)]
                async fn asset(cx: &Cx) -> Result<Response> {
                    crate::server::asset_route(cx).await
                }
            }
        }
    }
}
