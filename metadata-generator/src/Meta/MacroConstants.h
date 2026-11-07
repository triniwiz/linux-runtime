#pragma once

#include "Filters/ModulesBlacklist.h"
#include "MetaEntities.h"
#include <clang/Lex/HeaderSearch.h>
#include <clang/Lex/Preprocessor.h>
#include <memory>
#include <unordered_set>

namespace Meta {

struct MacroCandidate {
    std::string name;
    clang::Module* module;
    std::string fileName;
};

/// Object-like macros from the parsed modules that may be constant expressions.
std::vector<MacroCandidate> collectMacroCandidates(clang::Preprocessor& preprocessor, clang::HeaderSearch& headerSearch, ModulesBlacklist& blacklist, const std::unordered_set<std::string>& taken);

/// Evaluates the candidates with clang (a second parse of `source` plus one
/// probe variable per macro); constants become unscoped enum constant metas
/// whose value is JS code: a number, a BigInt literal or a string.
std::vector<std::unique_ptr<EnumConstantMeta>> evaluateMacros(const std::vector<MacroCandidate>& candidates, const std::string& source, const std::vector<std::string>& clangArgs);

} // namespace Meta
