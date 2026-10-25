# Changelog

## [0.2.0](https://github.com/ieasybooks/tahweel-rs/compare/v0.1.1...v0.2.0) (2026-10-25)


### ⚠ BREAKING CHANGES

* **drive:** let Drive detect the OCR language

### Features

* **drive:** let Drive detect the OCR language ([90172ba](https://github.com/ieasybooks/tahweel-rs/commit/90172baf670e90efb86f1f56957a04399d0359da))
* **drive:** load service-account keys from JSON text ([3f6a885](https://github.com/ieasybooks/tahweel-rs/commit/3f6a885cbcf6ec5a9160551dbffd5f0a73bc6493))
* **drive:** load service-account keys from JSON text ([161c7c7](https://github.com/ieasybooks/tahweel-rs/commit/161c7c7249fbe74038d907cf7b0cc4af14dac84b))
* **drive:** lower concurrency when requests time out ([899f897](https://github.com/ieasybooks/tahweel-rs/commit/899f89774c4f528421da1c26e1228dbdae15a595))
* **drive:** lower concurrency when requests time out ([3985656](https://github.com/ieasybooks/tahweel-rs/commit/398565658cade8f3408ff522fd1ac3cc12631bfe))

## [0.1.1](https://github.com/ieasybooks/tahweel-rs/compare/v0.1.0...v0.1.1) (2026-10-25)


### Bug Fixes

* **drive:** stop cutting off uploads that take more than a minute ([9503bb9](https://github.com/ieasybooks/tahweel-rs/commit/9503bb96dcf0151bd9f2da305a7363f2653994e2))
* **drive:** stop cutting off uploads that take more than a minute ([ebe0f2d](https://github.com/ieasybooks/tahweel-rs/commit/ebe0f2d4cbc157e7020862f123274ae70272720c))

## 0.1.0 (2026-10-25)


### Features

* **cli:** add the tahweel command ([12755ee](https://github.com/ieasybooks/tahweel-rs/commit/12755ee4db1ec12eb5236548740939adc1200885))
* **core:** add the recognizer interface ([109872e](https://github.com/ieasybooks/tahweel-rs/commit/109872ec5f0f62af1061b012938e71f50fcdbd8a))
* **core:** discover inputs and write txt, docx and json ([e26855e](https://github.com/ieasybooks/tahweel-rs/commit/e26855e6cff20a04d9d2723b0c3e58ebaadfb840))
* **core:** render pages and run the OCR pipeline ([fe9956d](https://github.com/ieasybooks/tahweel-rs/commit/fe9956d4a3dcd1502e1bc9432a40023f76a8b211))
* **drive:** recognize pages with Google Drive and service accounts ([2a0074e](https://github.com/ieasybooks/tahweel-rs/commit/2a0074ec29d75f20334feb0c4feccf8f5c06b0f3))
* **drive:** sign in with Google in the browser ([7f366d7](https://github.com/ieasybooks/tahweel-rs/commit/7f366d7b3f0d167d52f9b6ecdcc8eb516380728a))
* use defaults measured on Arabic book pages ([9c16f0a](https://github.com/ieasybooks/tahweel-rs/commit/9c16f0a4ed62fe29c175fc08029275c37fa7c8e8))
