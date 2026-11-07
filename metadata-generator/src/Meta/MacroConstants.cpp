#include "MacroConstants.h"
#include <clang/AST/ASTConsumer.h>
#include <clang/AST/ASTContext.h>
#include <clang/AST/Expr.h>
#include <clang/Frontend/CompilerInstance.h>
#include <clang/Frontend/FrontendAction.h>
#include <clang/Tooling/Tooling.h>
#include <cmath>
#include <iomanip>
#include <optional>
#include <sstream>

namespace Meta {

static const char* PROBE = "__ns_macro_";

/// Tokens that can appear in a constant expression (casts included).
static bool isExpressionToken(const clang::Token& token)
{
    switch (token.getKind()) {
    case clang::tok::numeric_constant:
    case clang::tok::char_constant:
    case clang::tok::string_literal:
    case clang::tok::utf8_string_literal:
    case clang::tok::identifier:
    case clang::tok::l_paren:
    case clang::tok::r_paren:
    case clang::tok::plus:
    case clang::tok::minus:
    case clang::tok::star:
    case clang::tok::slash:
    case clang::tok::percent:
    case clang::tok::lessless:
    case clang::tok::greatergreater:
    case clang::tok::amp:
    case clang::tok::pipe:
    case clang::tok::caret:
    case clang::tok::tilde:
    case clang::tok::exclaim:
    case clang::tok::less:
    case clang::tok::greater:
    case clang::tok::lessequal:
    case clang::tok::greaterequal:
    case clang::tok::equalequal:
    case clang::tok::exclaimequal:
    case clang::tok::ampamp:
    case clang::tok::pipepipe:
    case clang::tok::question:
    case clang::tok::colon:
    case clang::tok::kw_int:
    case clang::tok::kw_unsigned:
    case clang::tok::kw_signed:
    case clang::tok::kw_long:
    case clang::tok::kw_short:
    case clang::tok::kw_char:
    case clang::tok::kw_float:
    case clang::tok::kw_double:
    case clang::tok::kw_sizeof:
    case clang::tok::kw_const:
    case clang::tok::kw__Bool:
        return true;
    default:
        return false;
    }
}

/// Names a global can't have in JS.
static bool isReservedWord(const std::string& name)
{
    static const std::unordered_set<std::string> words{ "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do", "else", "enum", "export", "extends", "false", "finally", "for", "function", "if", "import", "in", "instanceof", "new", "null", "return", "super", "switch", "this", "throw", "true", "try", "typeof", "var", "void", "while", "with", "yield", "let", "static", "implements", "interface", "package", "private", "protected", "public", "await", "undefined", "NaN", "Infinity" };
    return words.count(name) > 0;
}

std::vector<MacroCandidate> collectMacroCandidates(clang::Preprocessor& preprocessor, clang::HeaderSearch& headerSearch, ModulesBlacklist& blacklist, const std::unordered_set<std::string>& taken)
{
    clang::SourceManager& sourceManager = preprocessor.getSourceManager();
    std::vector<MacroCandidate> candidates;
    for (const auto& entry : preprocessor.macros()) {
        const clang::IdentifierInfo* identifier = entry.first;
        std::string name = identifier->getName().str();
        // reserved names: include guards, compiler internals
        if (name.rfind("__", 0) == 0 || taken.count(name) || isReservedWord(name)) {
            continue;
        }
        const clang::MacroInfo* macro = preprocessor.getMacroInfo(identifier);
        if (!macro || !macro->isObjectLike() || macro->isBuiltinMacro() || macro->getNumTokens() == 0) {
            continue;
        }
        if (!std::all_of(macro->tokens_begin(), macro->tokens_end(), isExpressionToken)) {
            continue;
        }
        // self referencing (#define X X) can't be a constant
        if (macro->getNumTokens() == 1 && macro->getReplacementToken(0).getIdentifierInfo() == identifier) {
            continue;
        }
        clang::SourceLocation location = sourceManager.getFileLoc(macro->getDefinitionLoc());
        clang::OptionalFileEntryRef file = sourceManager.getFileEntryRefForID(sourceManager.getFileID(location));
        if (!file) {
            continue;
        }
        clang::Module* module = headerSearch.findModuleForHeader(*file).getModule();
        std::string whitelistRule, blacklistRule;
        // clang's own headers (stdbool, limits, float): libc has the useful ones
        if (!module || module->getTopLevelModuleName().startswith("_Builtin") || blacklist.shouldBlacklist(module->getFullModuleName(), name, whitelistRule, blacklistRule)) {
            continue;
        }
        candidates.push_back({ name, module, file->getName().str() });
    }
    std::sort(candidates.begin(), candidates.end(), [](const MacroCandidate& a, const MacroCandidate& b) { return a.name < b.name; });
    return candidates;
}

static std::string jsString(llvm::StringRef bytes)
{
    std::ostringstream out;
    out << '"';
    for (unsigned char c : bytes) {
        if (c == '"' || c == '\\') {
            out << '\\' << c;
        } else if (c < 0x20 || c == 0x7f) {
            out << "\\u" << std::hex << std::setw(4) << std::setfill('0') << (int)c << std::dec;
        } else {
            out << c;
        }
    }
    out << '"';
    return out.str();
}

static std::string jsDouble(double value)
{
    if (std::isnan(value)) {
        return "NaN";
    }
    if (std::isinf(value)) {
        return value > 0 ? "Infinity" : "-Infinity";
    }
    std::ostringstream out;
    out << std::setprecision(17) << value;
    return out.str();
}

namespace {
struct Value {
    std::string js;
    std::string tsType;
};

class ProbeConsumer : public clang::ASTConsumer {
public:
    explicit ProbeConsumer(std::vector<std::optional<Value>>& values)
        : _values(values)
    {
    }

    void HandleTranslationUnit(clang::ASTContext& context) override
    {
        size_t prefix = strlen(PROBE);
        for (clang::Decl* decl : context.getTranslationUnitDecl()->decls()) {
            auto* var = clang::dyn_cast<clang::VarDecl>(decl);
            if (!var || var->isInvalidDecl() || !var->getInit() || !var->getName().startswith(PROBE)) {
                continue;
            }
            size_t index = std::stoul(var->getName().substr(prefix).str());
            if (index < _values.size()) {
                _values[index] = evaluate(var->getInit(), context);
            }
        }
    }

private:
    static std::optional<Value> evaluate(const clang::Expr* init, clang::ASTContext& context)
    {
        if (init->isValueDependent() || init->containsErrors()) {
            return std::nullopt;
        }
        if (auto* string = clang::dyn_cast<clang::StringLiteral>(init->IgnoreParenImpCasts())) {
            if (string->isOrdinary() || string->isUTF8()) {
                return Value{ jsString(string->getString()), "string" };
            }
            return std::nullopt;
        }
        clang::Expr::EvalResult result;
        if (!init->EvaluateAsRValue(result, context) || result.HasSideEffects) {
            return std::nullopt;
        }
        const clang::APValue& value = result.Val;
        if (value.isInt()) {
            const llvm::APSInt& integer = value.getInt();
            llvm::SmallString<24> digits;
            integer.toString(digits, 10);
            // beyond Number.MAX_SAFE_INTEGER: BigInt
            bool big = integer.isSigned() ? integer.getSignificantBits() > 54 : integer.getActiveBits() > 53;
            return big ? Value{ std::string(digits) + "n", "bigint" } : Value{ std::string(digits), "number" };
        }
        if (value.isFloat()) {
            llvm::APFloat number = value.getFloat();
            bool lost;
            number.convert(llvm::APFloat::IEEEdouble(), llvm::APFloat::rmNearestTiesToEven, &lost);
            return Value{ jsDouble(number.convertToDouble()), "number" };
        }
        return std::nullopt;
    }

    std::vector<std::optional<Value>>& _values;
};

class ProbeAction : public clang::ASTFrontendAction {
public:
    explicit ProbeAction(std::vector<std::optional<Value>>& values)
        : _values(values)
    {
    }

    std::unique_ptr<clang::ASTConsumer> CreateASTConsumer(clang::CompilerInstance& compiler, llvm::StringRef) override
    {
        compiler.getPreprocessor().SetSuppressIncludeNotFoundError(true);
        // probes that aren't constants are expected to fail
        compiler.getDiagnostics().setSuppressAllDiagnostics(true);
        return std::make_unique<ProbeConsumer>(_values);
    }

private:
    std::vector<std::optional<Value>>& _values;
};
} // namespace

std::vector<std::unique_ptr<EnumConstantMeta>> evaluateMacros(const std::vector<MacroCandidate>& candidates, const std::string& source, const std::vector<std::string>& clangArgs)
{
    std::vector<std::unique_ptr<EnumConstantMeta>> metas;
    if (candidates.empty()) {
        return metas;
    }
    std::ostringstream probes;
    probes << source << "\n";
    for (size_t i = 0; i < candidates.size(); i++) {
        probes << "static __auto_type " << PROBE << i << " = (" << candidates[i].name << ");\n";
    }
    std::vector<std::optional<Value>> values(candidates.size());
    clang::tooling::runToolOnCodeWithArgs(std::make_unique<ProbeAction>(values), probes.str(), clangArgs, "umbrella.h", "metadata-generator");

    for (size_t i = 0; i < candidates.size(); i++) {
        if (!values[i]) {
            continue;
        }
        auto meta = std::make_unique<EnumConstantMeta>();
        meta->name = meta->jsName = candidates[i].name;
        meta->module = candidates[i].module;
        meta->fileName = candidates[i].fileName;
        meta->value = values[i]->js;
        meta->tsType = values[i]->tsType;
        metas.push_back(std::move(meta));
    }
    return metas;
}

} // namespace Meta
