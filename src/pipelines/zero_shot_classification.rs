// Copyright 2019-present, the HuggingFace Inc. team, The Google AI Language Team and Facebook, Inc.
// Copyright 2019-2020 Guillaume Becquin
// Copyright 2020 Maarten van Gompel
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//     http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! # Zero-shot classification pipeline
//! Performs zero-shot classification on input sentences with provided labels using a model fine-tuned for Natural Language Inference.
//! The default model is a BART model fine-tuned on a MNLI. From a list of input sequences to classify and a list of target labels,
//! single-class or multi-label classification is performed, translating the classification task to an inference task.
//! The default template for translation to inference task is `This example is about {}.`. This template can be updated to a more specific
//! value that may match better the use case, for example `This review is about a {product_class}`.
//!
//! - `predict` performs single-class classification (one and exactly one label must be true for each provided input)
//! - `predict_multilabel` performs multi-label classification (zero, one or more labels may be true for each provided input)
//!
//! ```no_run
//! # use rust_bert::pipelines::zero_shot_classification::ZeroShotClassificationModel;
//! # fn main() -> anyhow::Result<()> {
//! let sequence_classification_model = ZeroShotClassificationModel::new(Default::default())?;
//!  let input_sentence = "Who are you voting for in 2020?";
//!  let input_sequence_2 = "The prime minister has announced a stimulus package which was widely criticized by the opposition.";
//!  let candidate_labels = &["politics", "public health", "economics", "sports"];
//!  let output = sequence_classification_model.predict_multilabel(
//!      &[input_sentence, input_sequence_2],
//!      candidate_labels,
//!      None,
//!      128,
//!  );
//! # Ok(())
//! # }
//! ```
//!
//! outputs:
//! ```no_run
//! # use rust_bert::pipelines::sequence_classification::Label;
//! let output = [
//!     [
//!         Label {
//!             text: "politics".to_string(),
//!             score: 0.972,
//!             id: 0,
//!             sentence: 0,
//!         },
//!         Label {
//!             text: "public health".to_string(),
//!             score: 0.032,
//!             id: 1,
//!             sentence: 0,
//!         },
//!         Label {
//!             text: "economy".to_string(),
//!             score: 0.006,
//!             id: 2,
//!             sentence: 0,
//!         },
//!         Label {
//!             text: "sports".to_string(),
//!             score: 0.004,
//!             id: 3,
//!             sentence: 0,
//!         },
//!     ],
//!     [
//!         Label {
//!             text: "politics".to_string(),
//!             score: 0.943,
//!             id: 0,
//!             sentence: 1,
//!         },
//!         Label {
//!             text: "economy".to_string(),
//!             score: 0.985,
//!             id: 2,
//!             sentence: 1,
//!         },
//!         Label {
//!             text: "public health".to_string(),
//!             score: 0.0818,
//!             id: 1,
//!             sentence: 1,
//!         },
//!         Label {
//!             text: "sports".to_string(),
//!             score: 0.001,
//!             id: 3,
//!             sentence: 1,
//!         },
//!     ],
//! ]
//! .to_vec();
//! ```

use crate::albert::AlbertForSequenceClassification;
use crate::bart::{
    BartConfigResources, BartForSequenceClassification, BartMergesResources, BartModelResources,
    BartVocabResources,
};
use crate::bert::BertForSequenceClassification;
use crate::distilbert::DistilBertModelClassifier;
use crate::longformer::LongformerForSequenceClassification;
use crate::mobilebert::MobileBertForSequenceClassification;
use crate::pipelines::common::{ConfigOption, ModelType, TokenizerOption};
use crate::pipelines::sequence_classification::Label;
use crate::resources::{RemoteResource, Resource};
use crate::roberta::RobertaForSequenceClassification;
use crate::xlnet::XLNetForSequenceClassification;
use crate::RustBertError;
use rust_tokenizers::tokenizer::TruncationStrategy;
use rust_tokenizers::TokenizedInput;
use std::borrow::Borrow;
use std::ops::Deref;
use tch::kind::Kind::{Bool, Float};
use tch::nn::VarStore;
use tch::{nn, no_grad, Device, Tensor};

/// # Configuration for ZeroShotClassificationModel
/// Contains information regarding the model to load and device to place the model on.
pub struct ZeroShotClassificationConfig {
    /// Model type
    pub model_type: ModelType,
    /// Model weights resource (default: pretrained BERT model on CoNLL)
    pub model_resource: Resource,
    /// Config resource (default: pretrained BERT model on CoNLL)
    pub config_resource: Resource,
    /// Vocab resource (default: pretrained BERT model on CoNLL)
    pub vocab_resource: Resource,
    /// Merges resource (default: None)
    pub merges_resource: Option<Resource>,
    /// Automatically lower case all input upon tokenization (assumes a lower-cased model)
    pub lower_case: bool,
    /// Flag indicating if the tokenizer should strip accents (normalization). Only used for BERT / ALBERT models
    pub strip_accents: Option<bool>,
    /// Flag indicating if the tokenizer should add a white space before each tokenized input (needed for some Roberta models)
    pub add_prefix_space: Option<bool>,
    /// Device to place the model on (default: CUDA/GPU when available)
    pub device: Device,
}

impl ZeroShotClassificationConfig {
    /// Instantiate a new zero shot classification configuration of the supplied type.
    ///
    /// # Arguments
    ///
    /// * `model_type` - `ModelType` indicating the model type to load (must match with the actual data to be loaded!)
    /// * model - The `Resource` pointing to the model to load (e.g.  model.ot)
    /// * config - The `Resource' pointing to the model configuration to load (e.g. config.json)
    /// * vocab - The `Resource' pointing to the tokenizer's vocabulary to load (e.g.  vocab.txt/vocab.json)
    /// * vocab - An optional `Resource` tuple (`Option<Resource>`) pointing to the tokenizer's merge file to load (e.g.  merges.txt), needed only for Roberta.
    /// * lower_case - A `bool' indicating whether the tokenizer should lower case all input (in case of a lower-cased model)
    pub fn new(
        model_type: ModelType,
        model_resource: Resource,
        config_resource: Resource,
        vocab_resource: Resource,
        merges_resource: Option<Resource>,
        lower_case: bool,
        strip_accents: impl Into<Option<bool>>,
        add_prefix_space: impl Into<Option<bool>>,
    ) -> ZeroShotClassificationConfig {
        ZeroShotClassificationConfig {
            model_type,
            model_resource,
            config_resource,
            vocab_resource,
            merges_resource,
            lower_case,
            strip_accents: strip_accents.into(),
            add_prefix_space: add_prefix_space.into(),
            device: Device::cuda_if_available(),
        }
    }
}

impl Default for ZeroShotClassificationConfig {
    /// Provides a defaultSST-2 sentiment analysis model (English)
    fn default() -> ZeroShotClassificationConfig {
        ZeroShotClassificationConfig {
            model_type: ModelType::Bart,
            model_resource: Resource::Remote(RemoteResource::from_pretrained(
                BartModelResources::BART_MNLI,
            )),
            config_resource: Resource::Remote(RemoteResource::from_pretrained(
                BartConfigResources::BART_MNLI,
            )),
            vocab_resource: Resource::Remote(RemoteResource::from_pretrained(
                BartVocabResources::BART_MNLI,
            )),
            merges_resource: Some(Resource::Remote(RemoteResource::from_pretrained(
                BartMergesResources::BART_MNLI,
            ))),
            lower_case: false,
            strip_accents: None,
            add_prefix_space: None,
            device: Device::cuda_if_available(),
        }
    }
}

/// # Abstraction that holds one particular zero shot classification model, for any of the supported models
/// The models are using a classification architecture that should be trained on Natural Language Inference.
/// The models should output a Tensor of size > 2 in the label dimension, with the first logit corresponding
/// to contradiction and the last logit corresponding to entailment.
pub enum ZeroShotClassificationOption {
    /// Bart for Sequence Classification
    Bart(BartForSequenceClassification),
    /// Bert for Sequence Classification
    Bert(BertForSequenceClassification),
    /// DistilBert for Sequence Classification
    DistilBert(DistilBertModelClassifier),
    /// MobileBert for Sequence Classification
    MobileBert(MobileBertForSequenceClassification),
    /// Roberta for Sequence Classification
    Roberta(RobertaForSequenceClassification),
    /// XLMRoberta for Sequence Classification
    XLMRoberta(RobertaForSequenceClassification),
    /// Albert for Sequence Classification
    Albert(AlbertForSequenceClassification),
    /// XLNet for Sequence Classification
    XLNet(XLNetForSequenceClassification),
    /// Longformer for Sequence Classification
    Longformer(LongformerForSequenceClassification),
}

impl ZeroShotClassificationOption {
    /// Instantiate a new zero shot classification model of the supplied type.
    ///
    /// # Arguments
    ///
    /// * `model_type` - `ModelType` indicating the model type to load (must match with the actual data to be loaded)
    /// * `p` - `tch::nn::Path` path to the model file to load (e.g. model.ot)
    /// * `config` - A configuration (the model type of the configuration must be compatible with the value for
    /// `model_type`)
    pub fn new<'p, P>(
        model_type: ModelType,
        p: P,
        config: &ConfigOption,
    ) -> Result<Self, RustBertError>
    where
        P: Borrow<nn::Path<'p>>,
    {
        match model_type {
            ModelType::Bart => {
                if let ConfigOption::Bart(config) = config {
                    Ok(ZeroShotClassificationOption::Bart(
                        BartForSequenceClassification::new(p, config),
                    ))
                } else {
                    Err(RustBertError::InvalidConfigurationError(
                        "You can only supply a BartConfig for Bart!".to_string(),
                    ))
                }
            }
            ModelType::Bert => {
                if let ConfigOption::Bert(config) = config {
                    Ok(ZeroShotClassificationOption::Bert(
                        BertForSequenceClassification::new(p, config),
                    ))
                } else {
                    Err(RustBertError::InvalidConfigurationError(
                        "You can only supply a BertConfig for Bert!".to_string(),
                    ))
                }
            }
            ModelType::DistilBert => {
                if let ConfigOption::DistilBert(config) = config {
                    Ok(ZeroShotClassificationOption::DistilBert(
                        DistilBertModelClassifier::new(p, config),
                    ))
                } else {
                    Err(RustBertError::InvalidConfigurationError(
                        "You can only supply a DistilBertConfig for DistilBert!".to_string(),
                    ))
                }
            }
            ModelType::MobileBert => {
                if let ConfigOption::MobileBert(config) = config {
                    Ok(ZeroShotClassificationOption::MobileBert(
                        MobileBertForSequenceClassification::new(p, config),
                    ))
                } else {
                    Err(RustBertError::InvalidConfigurationError(
                        "You can only supply a MobileBertConfig for MobileBert!".to_string(),
                    ))
                }
            }
            ModelType::Roberta => {
                if let ConfigOption::Bert(config) = config {
                    Ok(ZeroShotClassificationOption::Roberta(
                        RobertaForSequenceClassification::new(p, config),
                    ))
                } else {
                    Err(RustBertError::InvalidConfigurationError(
                        "You can only supply a BertConfig for Roberta!".to_string(),
                    ))
                }
            }
            ModelType::XLMRoberta => {
                if let ConfigOption::Bert(config) = config {
                    Ok(ZeroShotClassificationOption::XLMRoberta(
                        RobertaForSequenceClassification::new(p, config),
                    ))
                } else {
                    Err(RustBertError::InvalidConfigurationError(
                        "You can only supply a BertConfig for Roberta!".to_string(),
                    ))
                }
            }
            ModelType::Albert => {
                if let ConfigOption::Albert(config) = config {
                    Ok(ZeroShotClassificationOption::Albert(
                        AlbertForSequenceClassification::new(p, config),
                    ))
                } else {
                    Err(RustBertError::InvalidConfigurationError(
                        "You can only supply an AlbertConfig for Albert!".to_string(),
                    ))
                }
            }
            ModelType::XLNet => {
                if let ConfigOption::XLNet(config) = config {
                    Ok(ZeroShotClassificationOption::XLNet(
                        XLNetForSequenceClassification::new(p, config).unwrap(),
                    ))
                } else {
                    Err(RustBertError::InvalidConfigurationError(
                        "You can only supply an AlbertConfig for Albert!".to_string(),
                    ))
                }
            }
            ModelType::Longformer => {
                if let ConfigOption::Longformer(config) = config {
                    Ok(ZeroShotClassificationOption::Longformer(
                        LongformerForSequenceClassification::new(p, config),
                    ))
                } else {
                    Err(RustBertError::InvalidConfigurationError(
                        "You can only supply a LongformerConfig for Longformer!".to_string(),
                    ))
                }
            }
            _ => Err(RustBertError::InvalidConfigurationError(format!(
                "Zero shot classification not implemented for {:?}!",
                model_type
            ))),
        }
    }

    /// Returns the `ModelType` for this SequenceClassificationOption
    pub fn model_type(&self) -> ModelType {
        match *self {
            Self::Bart(_) => ModelType::Bart,
            Self::Bert(_) => ModelType::Bert,
            Self::Roberta(_) => ModelType::Roberta,
            Self::XLMRoberta(_) => ModelType::Roberta,
            Self::DistilBert(_) => ModelType::DistilBert,
            Self::MobileBert(_) => ModelType::MobileBert,
            Self::Albert(_) => ModelType::Albert,
            Self::XLNet(_) => ModelType::XLNet,
            Self::Longformer(_) => ModelType::Longformer,
        }
    }

    /// Interface method to forward_t() of the particular models.
    pub fn forward_t(
        &self,
        input_ids: Option<Tensor>,
        mask: Option<Tensor>,
        token_type_ids: Option<Tensor>,
        position_ids: Option<Tensor>,
        input_embeds: Option<Tensor>,
        train: bool,
    ) -> Tensor {
        match *self {
            Self::Bart(ref model) => {
                model
                    .forward_t(
                        &input_ids.expect("`input_ids` must be provided for BART models"),
                        mask.as_ref(),
                        None,
                        None,
                        None,
                        train,
                    )
                    .decoder_output
            }
            Self::Bert(ref model) => {
                model
                    .forward_t(
                        input_ids,
                        mask,
                        token_type_ids,
                        position_ids,
                        input_embeds,
                        train,
                    )
                    .logits
            }
            Self::DistilBert(ref model) => {
                model
                    .forward_t(input_ids, mask, input_embeds, train)
                    .expect("Error in distilbert forward_t")
                    .logits
            }
            Self::MobileBert(ref model) => {
                model
                    .forward_t(
                        input_ids.as_ref(),
                        None,
                        None,
                        input_embeds,
                        mask.as_ref(),
                        train,
                    )
                    .expect("Error in mobilebert forward_t")
                    .logits
            }
            Self::Roberta(ref model) | Self::XLMRoberta(ref model) => {
                model
                    .forward_t(
                        input_ids,
                        mask,
                        token_type_ids,
                        position_ids,
                        input_embeds,
                        train,
                    )
                    .logits
            }
            Self::Albert(ref model) => {
                model
                    .forward_t(
                        input_ids,
                        mask,
                        token_type_ids,
                        position_ids,
                        input_embeds,
                        train,
                    )
                    .logits
            }
            Self::XLNet(ref model) => {
                model
                    .forward_t(
                        input_ids.as_ref(),
                        mask.as_ref(),
                        None,
                        None,
                        None,
                        token_type_ids.as_ref(),
                        input_embeds,
                        train,
                    )
                    .logits
            }
            Self::Longformer(ref model) => {
                model
                    .forward_t(
                        input_ids.as_ref(),
                        mask.as_ref(),
                        None,
                        token_type_ids.as_ref(),
                        position_ids.as_ref(),
                        input_embeds.as_ref(),
                        train,
                    )
                    .expect("Error in Longformer forward pass.")
                    .logits
            }
        }
    }
}

/// # ZeroShotClassificationModel for Zero Shot Classification
pub struct ZeroShotClassificationModel {
    tokenizer: TokenizerOption,
    zero_shot_classifier: ZeroShotClassificationOption,
    var_store: VarStore,
}

impl ZeroShotClassificationModel {
    /// Build a new `ZeroShotClassificationModel`
    ///
    /// # Arguments
    ///
    /// * `config` - `SequenceClassificationConfig` object containing the resource references (model, vocabulary, configuration) and device placement (CPU/GPU)
    ///
    /// # Example
    ///
    /// ```no_run
    /// # fn main() -> anyhow::Result<()> {
    /// use rust_bert::pipelines::sequence_classification::SequenceClassificationModel;
    ///
    /// let model = SequenceClassificationModel::new(Default::default())?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(
        config: ZeroShotClassificationConfig,
    ) -> Result<ZeroShotClassificationModel, RustBertError> {
        let config_path = config.config_resource.get_local_path()?;
        let vocab_path = config.vocab_resource.get_local_path()?;
        let weights_path = config.model_resource.get_local_path()?;
        let merges_path = if let Some(merges_resource) = &config.merges_resource {
            Some(merges_resource.get_local_path()?)
        } else {
            None
        };
        let device = config.device;

        let tokenizer = TokenizerOption::from_file(
            config.model_type,
            vocab_path.to_str().unwrap(),
            merges_path.as_deref().map(|path| path.to_str().unwrap()),
            config.lower_case,
            config.strip_accents,
            config.add_prefix_space,
        )?;
        let mut var_store = VarStore::new(device);
        let model_config = ConfigOption::from_file(config.model_type, config_path);
        let zero_shot_classifier =
            ZeroShotClassificationOption::new(config.model_type, &var_store.root(), &model_config)?;
        var_store.load(weights_path)?;
        Ok(ZeroShotClassificationModel {
            tokenizer,
            zero_shot_classifier,
            var_store,
        })
    }

    fn prepare_for_model<'a, S, T>(
        &self,
        inputs: S,
        labels: T,
        template: Option<Box<dyn Fn(&str) -> String>>,
        max_len: usize,
    ) -> (Tensor, Tensor)
    where
        S: AsRef<[&'a str]>,
        T: AsRef<[&'a str]>,
    {
        let label_sentences: Vec<String> = match template {
            Some(function) => labels
                .as_ref()
                .iter()
                .map(|label| function(label))
                .collect(),
            None => labels
                .as_ref()
                .iter()
                .map(|label| format!("This example is about {}.", label))
                .collect(),
        };

        let text_pair_list = inputs
            .as_ref()
            .iter()
            .flat_map(|input| {
                label_sentences
                    .iter()
                    .map(move |label_sentence| (input.deref(), label_sentence.as_str()))
            })
            .collect::<Vec<(&str, &str)>>();

        let tokenized_input: Vec<TokenizedInput> = self.tokenizer.encode_pair_list(
            text_pair_list.as_ref(),
            max_len,
            &TruncationStrategy::LongestFirst,
            0,
        );
        let max_len = tokenized_input
            .iter()
            .map(|input| input.token_ids.len())
            .max()
            .unwrap();
        let tokenized_input_tensors: Vec<tch::Tensor> =
            tokenized_input
                .iter()
                .map(|input| input.token_ids.clone())
                .map(|mut input| {
                    input.extend(vec![self.tokenizer.get_pad_id().expect(
                        "The Tokenizer used for zero shot classification should contain a PAD id"
                    ); max_len - input.len()]);
                    input
                })
                .map(|input| Tensor::of_slice(&(input)))
                .collect::<Vec<_>>();

        let tokenized_input_tensors =
            Tensor::stack(tokenized_input_tensors.as_slice(), 0).to(self.var_store.device());

        let mask = tokenized_input_tensors
            .ne(self
                .tokenizer
                .get_pad_id()
                .expect("The Tokenizer used for zero shot classification should contain a PAD id"))
            .to_kind(Bool);

        (tokenized_input_tensors, mask)
    }

    /// Zero shot classification with 1 (and exactly 1) true label.
    ///
    /// # Arguments
    ///
    /// * `input` - `&[&str]` Array of texts to classify.
    /// * `labels` - `&[&str]` Possible labels for the inputs.
    /// * `template` - `Option<Box<dyn Fn(&str) -> String>>` closure to build label propositions. If None, will default to `"This example is {}."`.
    /// * `max_length` -`usize` Maximum sequence length for the inputs. If needed, the input sequence will be truncated before the label template.
    ///
    /// # Returns
    ///
    /// * `Vec<Label>` containing with the most likely label for each input sentence.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # fn main() -> anyhow::Result<()> {
    /// use rust_bert::pipelines::zero_shot_classification::ZeroShotClassificationModel;
    ///
    /// let sequence_classification_model = ZeroShotClassificationModel::new(Default::default())?;
    ///
    /// let input_sentence = "Who are you voting for in 2020?";
    /// let input_sequence_2 = "The prime minister has announced a stimulus package which was widely criticized by the opposition.";
    /// let candidate_labels = &["politics", "public health", "economics", "sports"];
    ///
    /// let output = sequence_classification_model.predict(
    ///     &[input_sentence, input_sequence_2],
    ///     candidate_labels,
    ///     None,
    ///     128,
    /// );
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// outputs:
    /// ```no_run
    /// # use rust_bert::pipelines::sequence_classification::Label;
    /// let output = [
    ///     Label {
    ///         text: "politics".to_string(),
    ///         score: 0.959,
    ///         id: 0,
    ///         sentence: 0,
    ///     },
    ///     Label {
    ///         text: "economy".to_string(),
    ///         score: 0.642,
    ///         id: 2,
    ///         sentence: 1,
    ///     },
    /// ]
    /// .to_vec();
    /// ```
    pub fn predict<'a, S, T>(
        &self,
        inputs: S,
        labels: T,
        template: Option<Box<dyn Fn(&str) -> String>>,
        max_length: usize,
    ) -> Vec<Label>
    where
        S: AsRef<[&'a str]>,
        T: AsRef<[&'a str]>,
    {
        let num_inputs = inputs.as_ref().len();
        let (input_tensor, mask) =
            self.prepare_for_model(inputs.as_ref(), labels.as_ref(), template, max_length);
        let output = no_grad(|| {
            let output = self.zero_shot_classifier.forward_t(
                Some(input_tensor),
                Some(mask),
                None,
                None,
                None,
                false,
            );
            output.view((num_inputs as i64, labels.as_ref().len() as i64, -1i64))
        });

        let scores = output.softmax(1, Float).select(-1, -1);
        let label_indices = scores.as_ref().argmax(-1, true).squeeze1(1);
        let scores = scores
            .gather(1, &label_indices.unsqueeze(-1), false)
            .squeeze1(1);
        let label_indices = label_indices.iter::<i64>().unwrap().collect::<Vec<i64>>();
        let scores = scores.iter::<f64>().unwrap().collect::<Vec<f64>>();

        let mut output_labels: Vec<Label> = vec![];
        for sentence_idx in 0..label_indices.len() {
            let label_string = labels.as_ref()[label_indices[sentence_idx] as usize].to_string();
            let label = Label {
                text: label_string,
                score: scores[sentence_idx],
                id: label_indices[sentence_idx],
                sentence: sentence_idx,
            };
            output_labels.push(label)
        }
        output_labels
    }

    /// Zero shot multi-label classification with 0, 1 or no true label.
    ///
    /// # Arguments
    ///
    /// * `input` - `&[&str]` Array of texts to classify.
    /// * `labels` - `&[&str]` Possible labels for the inputs.
    /// * `template` - `Option<Box<dyn Fn(&str) -> String>>` closure to build label propositions. If None, will default to `"This example is about {}."`.
    /// * `max_length` -`usize` Maximum sequence length for the inputs. If needed, the input sequence will be truncated before the label template.
    ///
    /// # Returns
    ///
    /// * `Vec<Vec<Label>>` containing a vector of labels and their probability for each input text
    ///
    /// # Example
    ///
    /// ```no_run
    /// # fn main() -> anyhow::Result<()> {
    /// use rust_bert::pipelines::zero_shot_classification::ZeroShotClassificationModel;
    ///
    /// let sequence_classification_model = ZeroShotClassificationModel::new(Default::default())?;
    ///
    /// let input_sentence = "Who are you voting for in 2020?";
    /// let input_sequence_2 = "The central bank is meeting today to discuss monetary policy.";
    /// let candidate_labels = &["politics", "public health", "economics", "sports"];
    ///
    /// let output = sequence_classification_model.predict_multilabel(
    ///     &[input_sentence, input_sequence_2],
    ///     candidate_labels,
    ///     None,
    ///     128,
    /// );
    /// # Ok(())
    /// # }
    /// ```
    /// outputs:
    /// ```no_run
    /// # use rust_bert::pipelines::sequence_classification::Label;
    /// let output = [
    ///     [
    ///         Label {
    ///             text: "politics".to_string(),
    ///             score: 0.972,
    ///             id: 0,
    ///             sentence: 0,
    ///         },
    ///         Label {
    ///             text: "public health".to_string(),
    ///             score: 0.032,
    ///             id: 1,
    ///             sentence: 0,
    ///         },
    ///         Label {
    ///             text: "economy".to_string(),
    ///             score: 0.006,
    ///             id: 2,
    ///             sentence: 0,
    ///         },
    ///         Label {
    ///             text: "sports".to_string(),
    ///             score: 0.004,
    ///             id: 3,
    ///             sentence: 0,
    ///         },
    ///     ],
    ///     [
    ///         Label {
    ///             text: "politics".to_string(),
    ///             score: 0.975,
    ///             id: 0,
    ///             sentence: 1,
    ///         },
    ///         Label {
    ///             text: "economy".to_string(),
    ///             score: 0.852,
    ///             id: 2,
    ///             sentence: 1,
    ///         },
    ///         Label {
    ///             text: "public health".to_string(),
    ///             score: 0.0818,
    ///             id: 1,
    ///             sentence: 1,
    ///         },
    ///         Label {
    ///             text: "sports".to_string(),
    ///             score: 0.001,
    ///             id: 3,
    ///             sentence: 1,
    ///         },
    ///     ],
    /// ]
    /// .to_vec();
    /// ```
    pub fn predict_multilabel<'a, S, T>(
        &self,
        inputs: S,
        labels: T,
        template: Option<Box<dyn Fn(&str) -> String>>,
        max_length: usize,
    ) -> Vec<Vec<Label>>
    where
        S: AsRef<[&'a str]>,
        T: AsRef<[&'a str]>,
    {
        let num_inputs = inputs.as_ref().len();
        let (input_tensor, mask) =
            self.prepare_for_model(inputs.as_ref(), labels.as_ref(), template, max_length);
        let output = no_grad(|| {
            let output = self.zero_shot_classifier.forward_t(
                Some(input_tensor),
                Some(mask),
                None,
                None,
                None,
                false,
            );
            output.view((num_inputs as i64, labels.as_ref().len() as i64, -1i64))
        });
        let scores = output.slice(-1, 0, 3, 2).softmax(-1, Float).select(-1, -1);

        let mut output_labels = vec![];
        for sentence_idx in 0..num_inputs {
            let mut sentence_labels = vec![];

            for (label_index, score) in scores
                .select(0, sentence_idx as i64)
                .iter::<f64>()
                .unwrap()
                .enumerate()
            {
                let label_string = labels.as_ref()[label_index].to_string();
                let label = Label {
                    text: label_string,
                    score,
                    id: label_index as i64,
                    sentence: sentence_idx,
                };
                sentence_labels.push(label);
            }
            output_labels.push(sentence_labels);
        }
        output_labels
    }
}
#[cfg(test)]
mod test {
    use super::*;

    #[test]
    #[ignore] // no need to run, compilation is enough to verify it is Send
    fn test() {
        let config = ZeroShotClassificationConfig::default();
        let _: Box<dyn Send> = Box::new(ZeroShotClassificationModel::new(config));
    }
}
