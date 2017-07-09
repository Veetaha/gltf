
// Copyright 2017 The gltf Library Developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use futures::future;
use import::config;
use json;

use futures::{BoxFuture, Future};
use image_crate::{load_from_memory, load_from_memory_with_format};
use image_crate::ImageFormat::{JPEG as Jpeg, PNG as Png};
use import::{Config, Error, Source};
use root::Root;
use std::boxed::Box;
use std::io::Cursor;
use validation::Validate;

use {AsyncData, Gltf};

fn source_buffers(
    json: &json::Root,
    source: &Source,
) -> Vec<AsyncData> {
    json.buffers
        .iter()
        .map(|entry| {
            let uri = entry.uri.as_ref().unwrap();
            let future = source
                .source_external_data(uri)
                .boxed()
                .shared();
            AsyncData::full(future)
        })
        .collect()
}

fn source_images(
    json: &json::Root,
    buffers: &[AsyncData],
    source: &Source,
) -> Vec<AsyncData> {
    enum Type<'a> {
        Borrowed {
            buffer_index: usize,
            offset: usize,
            len: usize,
        },
        Owned {
            uri: &'a str,
        },
    }
    let mut images = vec![];
    for entry in &json.images {
        let format = entry.mime_type.as_ref().map(|x| match x.0.as_str() {
            "image/jpeg" => Jpeg,
            "image/png" => Png,
            _ => unreachable!(),
        });
        let ty = if let Some(uri) = entry.uri.as_ref() {
            Type::Owned {
                uri: uri,
            }
        } else if let Some(index) = entry.buffer_view.as_ref() {
            let buffer_view = &json.buffer_views[index.value()];
            Type::Borrowed {
                buffer_index: buffer_view.buffer.value(),
                offset: buffer_view.byte_offset as usize,
                len: buffer_view.byte_length as usize,
            }
        } else {
            unreachable!()
        };
        let future = match ty {
            Type::Owned {
                uri,
            } => {
                source
                    .source_external_data(uri)
                    .and_then(move |data| {
                        if let Some(format) = format {
                            match load_from_memory_with_format(&data, format) {
                                Ok(image) => {
                                    let pixels = image
                                        .raw_pixels()
                                        .into_boxed_slice();
                                    future::ok(pixels)
                                },
                                Err(err) => {
                                    future::err(Error::Decode(err))
                                },
                            }
                        } else {
                            match load_from_memory(&data) {
                                Ok(image) => {
                                    let pixels = image
                                        .raw_pixels()
                                        .into_boxed_slice();
                                    future::ok(pixels)
                                },
                                Err(err) => {
                                    future::err(Error::Decode(err))
                                },
                            }
                        }
                    })
                    .boxed()
                    .shared()
            },
            Type::Borrowed {
                buffer_index,
                offset,
                len,
            } => {
                buffers[buffer_index]
                    .clone()
                    .map_err(Error::LazyLoading)
                    .and_then(move |data| {
                        let slice = &data[offset..(offset + len)];
                        match load_from_memory_with_format(slice, format.unwrap()) {
                            Ok(image) => {
                                let pixels = image
                                    .raw_pixels()
                                    .into_boxed_slice();
                                future::ok(pixels)
                            },
                            Err(err) => {
                                future::err(Error::Decode(err))
                            },
                        }
                    })
                    .boxed()
                    .shared()
            },
        };
        images.push(AsyncData::full(future));
    }
    images
}

fn validate(
    json: json::Root,
    validation_strategy: config::ValidationStrategy,
) -> BoxFuture<json::Root, Error> {
    match validation_strategy {
        config::ValidationStrategy::Skip => {
            future::ok(json).boxed()
        },
        config::ValidationStrategy::Minimal => {
            future::lazy(move || {
                let mut errs = vec![];
                json.validate_minimally(
                    &json,
                    || json::Path::new(),
                    &mut |path, err| errs.push((path(), err)),
                );
                if errs.is_empty() {
                    future::ok(json)
                } else {
                    future::err(Error::Validation(errs))
                }
            }).boxed() 
        },
        config::ValidationStrategy::Complete => {
            future::lazy(move || {
                let mut errs = vec![];
                json.validate_completely(
                    &json,
                    || json::Path::new(),
                    &mut |path, err| errs.push((path(), err)),
                );
                if errs.is_empty() {
                    future::ok(json)
                } else {
                    future::err(Error::Validation(errs))
                }
            }).boxed() 
        },
    }
}

pub fn import<S: Source>(
    data: Box<[u8]>,
    source: S,
    config: Config,
) -> BoxFuture<Gltf, Error> {
    let gltf = future::lazy(move || {
        json::from_reader(Cursor::new(data)).map_err(Error::MalformedJson)
    })
        .and_then(move |json| {
            let config = config;
            validate(json, config.validation_strategy)
        })
        .and_then(move |json| {
            let source = source;
            let buffers = source_buffers(&json, &source);
            future::ok((json, source, buffers))
        })
        .and_then(|(json, source, buffers)| {
            let images = source_images(&json, &buffers, &source);
            future::ok((json, buffers, images))
        })
        .and_then(|(json, buffers, images)| {
            future::ok(Gltf::new(Root::new(json), buffers, images))
        });
    Box::new(gltf)
}
