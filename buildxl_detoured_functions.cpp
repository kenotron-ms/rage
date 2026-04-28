BuildXL/Public/Src/Sandbox/Windows/DetoursServices/DetouredFunctions.cpp at main · microsoft/BuildXL · GitHub
Skip to content
Navigation Menu
Toggle navigation
Sign in
Appearance settings
PlatformAI CODE CREATIONGitHub CopilotWrite better code with AIGitHub SparkBuild and deploy intelligent appsGitHub ModelsManage and compare promptsMCP RegistryNewIntegrate external toolsDEVELOPER WORKFLOWSActionsAutomate any workflowCodespacesInstant dev environmentsIssuesPlan and track workCode ReviewManage code changesAPPLICATION SECURITYGitHub Advanced SecurityFind and fix vulnerabilitiesCode securitySecure your code as you buildSecret protectionStop leaks before they startEXPLOREWhy GitHubDocumentationBlogChangelogMarketplaceView all featuresSolutionsBY COMPANY SIZEEnterprisesSmall and medium teamsStartupsNonprofitsBY USE CASEApp ModernizationDevSecOpsDevOpsCI/CDView all use casesBY INDUSTRYHealthcareFinancial servicesManufacturingGovernmentView all industriesView all solutionsResourcesEXPLORE BY TOPICAISoftware DevelopmentDevOpsSecurityView all topicsEXPLORE BY TYPECustomer storiesEvents & webinarsEbooks & reportsBusiness insightsGitHub SkillsSUPPORT & SERVICESDocumentationCustomer supportCommunity forumTrust centerPartnersView all resourcesOpen SourceCOMMUNITYGitHub SponsorsFund open source developersPROGRAMSSecurity LabMaintainer CommunityAcceleratorGitHub StarsArchive ProgramREPOSITORIESTopicsTrendingCollectionsEnterpriseENTERPRISE SOLUTIONSEnterprise platformAI-powered developer platformAVAILABLE ADD-ONSGitHub Advanced SecurityEnterprise-grade security featuresCopilot for BusinessEnterprise-grade AI featuresPremium SupportEnterprise-grade 24/7 supportPricing
Search or jump to...
Search code, repositories, users, issues, pull requests...
Search
Clear
Search syntax tips
Provide feedback
We read every piece of feedback, and take your input very seriously.
Include my email address so I can be contacted
Cancel
Submit feedback
Saved searches
Use saved searches to filter your results more quickly
Name
Query
To see all available qualifiers, see our documentation.
Cancel
Create saved search
Sign in
Sign up
Appearance settings
Resetting focus
You signed in with another tab or window. Reload to refresh your session.
You signed out in another tab or window. Reload to refresh your session.
You switched accounts on another tab or window. Reload to refresh your session.
Dismiss alert
microsoft
/
BuildXL
Public
Notifications
You must be signed in to change notification settings
Fork
157
Star
1k
Code
Issues
19
Pull requests
4
Models
Security and quality
0
Insights
Additional navigation options
Code
Issues
Pull requests
Models
Security and quality
Insights
FilesExpand file tree mainBreadcrumbsBuildXL/Public/Src/Sandbox/Windows/DetoursServices/DetouredFunctions.cppCopy pathBlameMore file actionsBlameMore file actions Latest commit HistoryHistoryHistory7516 lines (6529 loc) · 287 KB mainBreadcrumbsBuildXL/Public/Src/Sandbox/Windows/DetoursServices/DetouredFunctions.cppTopFile metadata and controlsCodeBlame7516 lines (6529 loc) · 287 KBRawCopy raw fileDownload raw fileOpen symbols panelEdit and raw actions1234567891011121314151617181920212223242526272829303132333435363738394041424344454647484950515253545556575859606162636465666768697071727374757677787980818283848586878889909192939495969798991001011021031041051061071081091101111121131141151161171181191201211221231241251261271281291301311321331341351361371381391401411421431441451461471481491501511521531541551561571581591601611621631641651661671681691701711721731741751761771781791801811821831841851861871881891901911921931941951961971981992002012022032042052062072082092102112122132142152162172182192202212222232242252262272282292302312322332342352362372382392402412422432442452462472482492502512522532542552562572582592602612622632642652662672682692702712722732742752762772782792802812822832842852862872882892902912922932942952962972982993003013023033043053063073083093103113123133143153163173183193203213223233243253263273283293303313323333343353363373383393403413423433443453463473483493503513523533543553563573583593603613623633643653663673683693703713723733743753763773783793803813823833843853863873883893903913923933943953963973983994004014024034044054064074084094104114124134144154164174184194204214224234244254264274284294304314324334344354364374384394404414424434444454464474484494504514524534544554564574584594604614624634644654664674684694704714724734744754764774784794804814824834844854864874884894904914924934944954964974984995005015025035045055065075085095105115125135145155165175185195205215225235245255265275285295305315325335345355365375385395405415425435445455465475485495505515525535545555565575585595605615625635645655665675685695705715725735745755765775785795805815825835845855865875885895905915925935945955965975985996006016026036046056066076086096106116126136146156166176186196206216226236246256266276286296306316326336346356366376386396406416426436446456466476486496506516526536546556566576586596606616626636646656666676686696706716726736746756766776786796806816826836846856866876886896906916926936946956966976986997007017027037047057067077087097107117127137147157167177187197207217227237247257267277287297307317327337347357367377387397407417427437447457467477487497507517527537547557567577587597607617627637647657667677687697707717727737747757767777787797807817827837847857867877887897907917927937947957967977987998008018028038048058068078088098108118128138148158168178188198208218228238248258268278288298308318328338348358368378388398408418428438448458468478488498508518528538548558568578588598608618628638648658668678688698708718728738748758768778788798808818828838848858868878888898908918928938948958968978988999009019029039049059069079089099109119129139149159169179189199209219229239249259269279289299309319329339349359369379389399409419429439449459469479489499509519529539549559569579589599609619629639649659669679689699709719729739749759769779789799809819829839849859869879889899909919929939949959969979989991000﻿// Copyright (c) Microsoft. All rights reserved.// Licensed under the MIT license. See LICENSE file in the project root for full license information.
#include "stdafx.h"
#include "DetouredFunctions.h"#include "DetouredScope.h"#include "HandleOverlay.h"#include "MetadataOverrides.h"#include "ResolvedPathCache.h"#include "SendReport.h"#include "StringOperations.h"#include "SubstituteProcessExecution.h"#include "UnicodeConverter.h"
#include <Pathcch.h>
using std::map;using std::vector;using std::wstring;
#if _MSC_VER >= 1200#pragma warning(disable:26812) // Disable: The enum type ‘X’ is unscoped warnings originating from the WinSDK#endif
// ----------------------------------------------------------------------------// FUNCTION DEFINITIONS// ----------------------------------------------------------------------------
#define IMPLEMENTED(x) // bookeeping to remember which functions have been fully implemented and which still need to be done#define RETRY_DETOURING_PROCESS_COUNT 5 // How many times to retry detouring a process.#define RETRY_DETOURING_PROCESS_SLEEP_MS 1000 // How long to sleep between retries.#define DETOURS_STATUS_ACCESS_DENIED (NTSTATUS)0xC0000022L;#define INITIAL_REPARSE_DATA_BUILDXL_DETOURS_BUFFER_SIZE_FOR_FILE_NAMES 1024#define SYMLINK_FLAG_RELATIVE 0x00000001
#define _MAX_EXTENDED_PATH_LENGTH 32768 // see https://docs.microsoft.com/en-us/cpp/c-runtime-library/path-field-limits?view=vs-2019#define _MAX_EXTENDED_DIR_LENGTH (_MAX_EXTENDED_PATH_LENGTH - _MAX_DRIVE - _MAX_FNAME - _MAX_EXT - 4)
#define NTQUERYDIRECTORYFILE_MIN_BUFFER_SIZE 4096
static bool IgnoreFullReparsePointResolvingForPath(const PolicyResult& policyResult){ return IgnoreFullReparsePointResolving() && !policyResult.EnableFullReparsePointParsing();}
/// <summary>/// Given a policy result, get the level of the file path where the path should start to be checked for reparse points./// d: is level 0, d:\a is level 1, etc.../// Every level >= the returned level should be checked for a reparse point./// If a reparse point is found, all levels of the newly resolved path should be checked for reparse points again./// Calls <code>IgnoreFullReparsePointResolving</code> and <code>PolicyResult.GetFirstLevelForFileAccessPolicy</code> to determine the level./// </summary>static size_t GetLevelToEnableFullReparsePointParsing(const PolicyResult& policyResult){ return IgnoreFullReparsePointResolving()
? policyResult.FindLowestConsecutiveLevelThatStillHasProperty(FileAccessPolicy::FileAccessPolicy_EnableFullReparsePointParsing)
: 0;}
/// <summary>/// Checks if a file is a reparse point by calling <code>GetFileAttributesW</code>./// </summary>static bool IsReparsePoint(_In_ LPCWSTR lpFileName, _In_ HANDLE hFile){
DWORD lastError = GetLastError(); if (hFile != INVALID_HANDLE_VALUE)
{
BY_HANDLE_FILE_INFORMATION fileInfo;
BOOL result = GetFileInformationByHandle(hFile, &fileInfo); if (result)
{ SetLastError(lastError); return (fileInfo.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0;
}
}
DWORD attributes; bool result = lpFileName != nullptr
&& ((attributes = GetFileAttributesW(lpFileName)) != INVALID_FILE_ATTRIBUTES)
&& ((attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0);
SetLastError(lastError);
return result;}
/// <summary>/// Gets reparse point type of a file name by querying <code>dwReserved0</code> field of <code>WIN32_FIND_DATA</code>./// </summary>static DWORD GetReparsePointType(_In_ LPCWSTR lpFileName, _In_ HANDLE hFile){
DWORD ret = 0;
DWORD lastError = GetLastError();
if (IsReparsePoint(lpFileName, hFile))
{
WIN32_FIND_DATA findData;
HANDLE findDataHandle = FindFirstFileW(lpFileName, &findData); if (findDataHandle != INVALID_HANDLE_VALUE)
{
ret = findData.dwReserved0; FindClose(findDataHandle);
}
}
SetLastError(lastError); return ret;}
/// <summary>/// Checks if a reparse point type is actionable, i.e., it is either <code>IO_REPARSE_TAG_SYMLINK</code> or <code>IO_REPARSE_TAG_MOUNT_POINT</code>./// </summary>static bool IsActionableReparsePointType(_In_ const DWORD reparsePointType){ return reparsePointType == IO_REPARSE_TAG_SYMLINK || reparsePointType == IO_REPARSE_TAG_MOUNT_POINT;}
/// <summary>/// Checks if the flags or attributes field contains the reparse point flag./// </summary>static bool FlagsAndAttributesContainReparsePointFlag(_In_ DWORD dwFlagsAndAttributes){ return (dwFlagsAndAttributes & FILE_FLAG_OPEN_REPARSE_POINT) != 0;}
/// <summary>/// Check if file access is trying to access reparse point target./// </summary>static bool AccessReparsePointTarget(
_In_
LPCWSTR
lpFileName,
_In_
DWORD
dwFlagsAndAttributes,
_In_
HANDLE
hFile){ return !FlagsAndAttributesContainReparsePointFlag(dwFlagsAndAttributes) && IsReparsePoint(lpFileName, hFile);}
/// <summary>/// Gets the final full path by handle./// </summary>/// <remarks>/// This function encapsulates calls to <code>GetFinalPathNameByHandleW</code> and allocates memory as needed./// </remarks>static DWORD DetourGetFinalPathByHandle(_In_ HANDLE hFile, _Inout_ wstring& fullPath){ // First, try with a fixed-sized buffer which should be good enough for all practical cases. wchar_t wszBuffer[MAX_PATH];
DWORD nBufferLength = std::extent<decltype(wszBuffer)>::value;
DWORD result = GetFinalPathNameByHandleW(hFile, wszBuffer, nBufferLength, FILE_NAME_NORMALIZED); if (result == 0)
{
DWORD ret = GetLastError(); return ret;
}
if (result < nBufferLength)
{ // The buffer was big enough. The return value indicates the length of the full path, NOT INCLUDING the terminating null character. // https://msdn.microsoft.com/en-us/library/windows/desktop/aa364962(v=vs.85).aspx
fullPath.assign(wszBuffer, static_cast<size_t>(result));
} else
{ // Second, if that buffer wasn't big enough, we try again with a dynamically allocated buffer with sufficient size. // Note that in this case, the return value indicates the required buffer length, INCLUDING the terminating null character. // https://msdn.microsoft.com/en-us/library/windows/desktop/aa364962(v=vs.85).aspx
unique_ptr<wchar_t[]> buffer(new wchar_t[result]); assert(buffer.get());
DWORD next_result = GetFinalPathNameByHandleW(hFile, buffer.get(), result, FILE_NAME_NORMALIZED); if (next_result == 0)
{
DWORD ret = GetLastError(); return ret;
}
if (next_result < result)
{
fullPath.assign(buffer.get(), next_result);
} else
{ return ERROR_NOT_ENOUGH_MEMORY;
}
}
return ERROR_SUCCESS;}
//////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////// Resolved path cache //////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////static void PathCache_Invalidate(const std::wstring& path, bool isDirectory, const PolicyResult& policyResult){ if (IgnoreReparsePoints() || IgnoreFullReparsePointResolvingForPath(policyResult))
{ return;
}
ResolvedPathCache::Instance().Invalidate(path, isDirectory);}
static const Possible<std::pair<std::wstring, DWORD>> PathCache_GetResolvedPathAndType(const std::wstring& path, const PolicyResult& policyResult){ if (IgnoreReparsePoints() || IgnoreFullReparsePointResolvingForPath(policyResult))
{
Possible<std::pair<std::wstring, DWORD>> p;
p.Found = false; return p;
}
return ResolvedPathCache::Instance().GetResolvedPathAndType(path);}
static bool PathCache_InsertResolvedPathWithType(const std::wstring& path, std::wstring& resolved, DWORD reparsePointType, const PolicyResult& policyResult){ if (IgnoreReparsePoints() || IgnoreFullReparsePointResolvingForPath(policyResult))
{ return true;
}
return ResolvedPathCache::Instance().InsertResolvedPathWithType(path, resolved, reparsePointType);}
static const Possible<bool> PathCache_GetResolvingCheckResult(const std::wstring& path, const PolicyResult& policyResult){ if (IgnoreReparsePoints() || IgnoreFullReparsePointResolvingForPath(policyResult))
{
Possible<bool> p;
p.Found = false; return p;
}
return ResolvedPathCache::Instance().GetResolvingCheckResult(path);}
static bool PathCache_InsertResolvingCheckResult(const std::wstring& path, bool result, const PolicyResult& policyResult){ if (IgnoreReparsePoints() || IgnoreFullReparsePointResolvingForPath(policyResult))
{ return true;
}
return ResolvedPathCache::Instance().InsertResolvingCheckResult(path, result);}
static bool PathCache_InsertResolvedPaths( const std::wstring& path, bool preserveLastReparsePointInPath,
std::shared_ptr<std::vector<std::wstring>>& insertionOrder,
std::shared_ptr<std::map<std::wstring, ResolvedPathType, CaseInsensitiveStringLessThan>>& resolvedPaths, const PolicyResult& policyResult){ if (IgnoreReparsePoints() || IgnoreFullReparsePointResolvingForPath(policyResult))
{ return true;
}
return ResolvedPathCache::Instance().InsertResolvedPaths(path, preserveLastReparsePointInPath, insertionOrder, resolvedPaths);}
static const Possible<ResolvedPathCacheEntries> PathCache_GetResolvedPaths(const std::wstring& path, bool preserveLastReparsePointInPath, const PolicyResult& policyResult){ if (IgnoreReparsePoints() || IgnoreFullReparsePointResolvingForPath(policyResult))
{
Possible<ResolvedPathCacheEntries> p;
p.Found = false; return p;
}
return ResolvedPathCache::Instance().GetResolvedPaths(path, preserveLastReparsePointInPath);}
/// <summary>/// Gets target name from <code>REPARSE_DATA_BUFFER</code>./// </summary>static void GetTargetNameFromReparseData(_In_ PREPARSE_DATA_BUFFER pReparseDataBuffer, _In_ DWORD reparsePointType, _Out_ wstring& name){ // In what follows, we first try to extract target name in the path buffer using the PrintNameOffset. // If it is empty or a single space, we try to extract target name from the SubstituteNameOffset. // This is pretty much guess-work. Tools like mklink and CreateSymbolicLink API insert the target name // from the PrintNameOffset. But others may use DeviceIoControl directly to insert the target name from SubstituteNameOffset. if (reparsePointType == IO_REPARSE_TAG_SYMLINK)
{
name.assign(
pReparseDataBuffer->SymbolicLinkReparseBuffer.PathBuffer + pReparseDataBuffer->SymbolicLinkReparseBuffer.PrintNameOffset / sizeof(WCHAR),
(size_t)pReparseDataBuffer->SymbolicLinkReparseBuffer.PrintNameLength / sizeof(WCHAR));
if (name.size() == 0 || name == L" ")
{
name.assign(
pReparseDataBuffer->SymbolicLinkReparseBuffer.PathBuffer + pReparseDataBuffer->SymbolicLinkReparseBuffer.SubstituteNameOffset / sizeof(WCHAR),
(size_t)pReparseDataBuffer->SymbolicLinkReparseBuffer.SubstituteNameLength / sizeof(WCHAR));
}
} else if (reparsePointType == IO_REPARSE_TAG_MOUNT_POINT)
{
name.assign(
pReparseDataBuffer->MountPointReparseBuffer.PathBuffer + pReparseDataBuffer->MountPointReparseBuffer.PrintNameOffset / sizeof(WCHAR),
(size_t)pReparseDataBuffer->MountPointReparseBuffer.PrintNameLength / sizeof(WCHAR));
if (name.size() == 0 || name == L" ")
{
name.assign(
pReparseDataBuffer->MountPointReparseBuffer.PathBuffer + pReparseDataBuffer->MountPointReparseBuffer.SubstituteNameOffset / sizeof(WCHAR),
(size_t)pReparseDataBuffer->MountPointReparseBuffer.SubstituteNameLength / sizeof(WCHAR));
}
}}
/// <summary>/// Sets target name on <code>REPARSE_DATA_BUFFER</code> for both print and substitute names. /// See https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_reparse_data_buffer for details./// Assumes the provided buffer is large enough to hold the target name./// Sets both the print name and the substitute name (depending on the consumer, one or both may be used)./// </summary>static void SetTargetNameFromReparseData(_In_ PREPARSE_DATA_BUFFER pReparseDataBuffer, _In_ DWORD reparsePointType, _In_ wstring& target){
USHORT targetLengthInBytes = (USHORT)(target.length() * sizeof(WCHAR));
// In both cases we put the print name at the beginning of the buffer, followed by the substitute name. // The order of these is up to the implementation. if (reparsePointType == IO_REPARSE_TAG_SYMLINK)
{ memcpy(
pReparseDataBuffer->SymbolicLinkReparseBuffer.PathBuffer,
target.c_str(),
targetLengthInBytes);
pReparseDataBuffer->SymbolicLinkReparseBuffer.PrintNameLength = targetLengthInBytes;
pReparseDataBuffer->SymbolicLinkReparseBuffer.PrintNameOffset = 0;
memcpy(
pReparseDataBuffer->SymbolicLinkReparseBuffer.PathBuffer + targetLengthInBytes / sizeof(WCHAR),
target.c_str(),
targetLengthInBytes);
pReparseDataBuffer->SymbolicLinkReparseBuffer.SubstituteNameLength = targetLengthInBytes;
pReparseDataBuffer->SymbolicLinkReparseBuffer.SubstituteNameOffset = targetLengthInBytes;
} else if (reparsePointType == IO_REPARSE_TAG_MOUNT_POINT)
{ memcpy(
pReparseDataBuffer->MountPointReparseBuffer.PathBuffer,
target.c_str(),
targetLengthInBytes);
pReparseDataBuffer->MountPointReparseBuffer.PrintNameLength = targetLengthInBytes;
pReparseDataBuffer->MountPointReparseBuffer.PrintNameOffset = 0;
memcpy(
pReparseDataBuffer->MountPointReparseBuffer.PathBuffer + targetLengthInBytes / sizeof(WCHAR),
target.c_str(),
targetLengthInBytes);
pReparseDataBuffer->MountPointReparseBuffer.SubstituteNameLength = targetLengthInBytes;
pReparseDataBuffer->MountPointReparseBuffer.SubstituteNameOffset = targetLengthInBytes;
}}
/// <summary>/// Get the reparse point target via DeviceIoControl/// </summary>static bool TryGetReparsePointTarget(_In_ const wstring& path, _In_ HANDLE hInput, _Inout_ wstring& target, const PolicyResult& policyResult){ bool isReparsePoint; auto result = PathCache_GetResolvingCheckResult(path, policyResult); if (result.Found)
{
isReparsePoint = result.Value;
} else
{
isReparsePoint = IsReparsePoint(path.c_str(), hInput); PathCache_InsertResolvingCheckResult(path, isReparsePoint, policyResult);
}
if (!isReparsePoint)
{ return false;
}
HANDLE hFile = INVALID_HANDLE_VALUE;
DWORD lastError = GetLastError();
DWORD reparsePointType = 0;
vector<char> buffer; bool status = false;
DWORD bufferSize = INITIAL_REPARSE_DATA_BUILDXL_DETOURS_BUFFER_SIZE_FOR_FILE_NAMES;
DWORD errorCode = ERROR_INSUFFICIENT_BUFFER;
DWORD bufferReturnedSize = 0;
PREPARSE_DATA_BUFFER pReparseDataBuffer;
auto io_result = PathCache_GetResolvedPathAndType(path, policyResult); if (io_result.Found)
{
#if MEASURE_REPARSEPOINT_RESOLVING_IMPACT InterlockedIncrement(&g_reparsePointTargetCacheHitCount);#endif // MEASURE_REPARSEPOINT_RESOLVING_IMPACT
target = io_result.Value.first;
reparsePointType = io_result.Value.second; if (reparsePointType == 0x0)
{ goto Epilogue;
} goto Success;
}
hFile = hInput != INVALID_HANDLE_VALUE
? hInput
: CreateFileW(
path.c_str(),
GENERIC_READ,
FILE_SHARE_READ | FILE_SHARE_DELETE | FILE_SHARE_WRITE, NULL,
OPEN_EXISTING,
FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS, NULL);
if (hFile == INVALID_HANDLE_VALUE)
{ goto Error;
}
while (errorCode == ERROR_MORE_DATA || errorCode == ERROR_INSUFFICIENT_BUFFER)
{
buffer.clear();
buffer.resize(bufferSize);
BOOL success = DeviceIoControl(
hFile,
FSCTL_GET_REPARSE_POINT, nullptr, 0,
buffer.data(),
bufferSize,
&bufferReturnedSize, nullptr);
if (success)
{
errorCode = ERROR_SUCCESS;
} else
{
bufferSize *= 2; // Increase buffer size
errorCode = GetLastError();
}
}
if (errorCode != ERROR_SUCCESS)
{ goto Error;
}
pReparseDataBuffer = (PREPARSE_DATA_BUFFER)buffer.data();
reparsePointType = pReparseDataBuffer->ReparseTag;
if (!IsActionableReparsePointType(reparsePointType))
{ goto Error;
}
GetTargetNameFromReparseData(pReparseDataBuffer, reparsePointType, target); PathCache_InsertResolvedPathWithType(path, target, reparsePointType, policyResult);
Success:
status = true; goto Epilogue;
Error:
// Also add dummy cache entry for paths that are not reparse points, so we can avoid calling DeviceIoControl repeatedly PathCache_InsertResolvedPathWithType(path, target, 0x0, policyResult);
Epilogue:
if (hFile != INVALID_HANDLE_VALUE && hFile != hInput)
{ CloseHandle(hFile);
}
SetLastError(lastError); return status;}
/// <summary>/// Checks if Detours should resolve all reparse points contained in a path./// </summary>/// <remarks>/// Given a path this function traverses it from left to right, checking if any components/// are of type 'reparse point'. As soon as an entry of that type is found, a positive result/// is returned, indicating that the path needs further processing to properly indicate all/// potential reparse point targets as file accesses upstream./// </remarks>static bool ShouldResolveReparsePointsInPath(
_In_
const CanonicalizedPath& path,
_In_
DWORD
dwFlagsAndAttributes,
_In_
const PolicyResult&
policyResult){ if (IgnoreReparsePoints())
{ return false;
}
if (IgnoreFullReparsePointResolvingForPath(policyResult))
{ return AccessReparsePointTarget(path.GetPathString(), dwFlagsAndAttributes, INVALID_HANDLE_VALUE);
}
// Untracked scopes never need full reparse point resolution if (policyResult.IndicateUntracked() && IgnoreUntrackedPathsInFullReparsePointResolving())
{ return false;
}
auto result = PathCache_GetResolvingCheckResult(path.GetPathStringWithoutTypePrefix(), policyResult); if (result.Found)
{#if MEASURE_REPARSEPOINT_RESOLVING_IMPACT InterlockedIncrement(&g_shouldResolveReparsePointCacheHitCount);#endif // MEASURE_REPARSEPOINT_RESOLVING_IMPACT return result.Value;
}
std::vector<std::wstring> atoms; int err = TryDecomposePath(path.GetPathStringWithoutTypePrefix(), atoms); if (err != 0)
{ Dbg(L"ShouldResolveReparsePointsInPath: _wsplitpath_s failed, not resolving path: %d", err); return false;
}
wstring target;
wstring resolver; size_t level = 0; size_t levelToEnforceReparsePointParsingFrom = GetLevelToEnableFullReparsePointParsing(policyResult); for (auto iter = atoms.begin(); iter != atoms.end(); iter++)
{
resolver.append(*iter);
if (level >= levelToEnforceReparsePointParsingFrom && TryGetReparsePointTarget(resolver, INVALID_HANDLE_VALUE, target, policyResult))
{ return true;
}
level++;
resolver.append(L"\\");
}
// remove the trailing backslash
resolver.pop_back();
if (level >= levelToEnforceReparsePointParsingFrom && TryGetReparsePointTarget(resolver, INVALID_HANDLE_VALUE, target, policyResult))
{ return true;
}
return false;}
// If the given path does not contain reparse points but the handle was open for write and open reparse point flag was passed,// then this may be the step previous to turning that directory into a reparse point. We don't detour the actual ioctl call, but conservatively we// invalidate the path from the cache. Otherwise, if the ioctl call actually happens, all subsequent reads on the path won't be resolved.static void InvalidateReparsePointCacheIfNeeded( bool pathContainsReparsePoints,
DWORD desiredAccess,
DWORD flagsAndAttributes, bool isDirectory, const wchar_t* path, const PolicyResult& policyResult){ if (!pathContainsReparsePoints
&& !IgnoreReparsePoints()
&& !IgnoreFullReparsePointResolvingForPath(policyResult)
&& WantsWriteAccess(desiredAccess)
&& FlagsAndAttributesContainReparsePointFlag(flagsAndAttributes))
{ PathCache_Invalidate(path, isDirectory, policyResult);
}}
//////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////// Symlink traversal utilities //////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////
/// <summary>/// Split paths into path atoms and insert them into <code>atoms</code> in reverse order./// </summary>static void SplitPathsReverse(_In_ const wstring& path, _Inout_ vector<wstring>& atoms){ size_t length = path.length();
if (length >= 2 && IsDirectorySeparator(path[length - 1]))
{ // Skip ending directory separator without trimming the path.
--length;
}
size_t rootLength = GetRootLength(path.c_str());
if (length <= rootLength)
{ return;
}
size_t i = length - 1;
wstring dir = path;
while (i >= rootLength)
{ while (i > rootLength && !IsDirectorySeparator(dir[i]))
{
--i;
}
if (i >= rootLength)
{
atoms.push_back(dir.substr(i));
}
dir = dir.substr(0, i);
if (i == 0)
{ break;
}
--i;
}
if (!dir.empty())
{
atoms.push_back(dir);
}}
/// <summary>/// Resolves a reparse point path with respect to its relative target./// </summary>/// <remarks>/// Given a reparse point path A\B\C and its relative target D\E\F, this method/// simply "combines" A\B and D\E\F. The symlink C is essentially replaced by the relative target D\E\F./// </remarks>static bool TryResolveRelativeTarget(
_Inout_ wstring& result,
_In_ const wstring& relativeTarget,
_In_ vector<wstring> *processed,
_In_ vector<wstring> *needToBeProcessed){ // Trim directory separator ending. if (result[result.length() - 1] == L'\\')
{
result = result.substr(0, result.length() - 1);
}
// Skip last path atom. size_t lastSeparator = result.find_last_of(L'\\'); if (lastSeparator == std::string::npos)
{ return false;
}
if (processed != nullptr)
{ if (processed->empty())
{ return false;
}
processed->pop_back();
}
// Handle '.' and '..' in the relative target. size_t pos = 0; size_t length = relativeTarget.length(); bool startWithDotSlash = length >= 2 && relativeTarget[pos] == L'.' && relativeTarget[pos + 1] == L'\\'; bool startWithDotDotSlash = length >= 3 && relativeTarget[pos] == L'.' && relativeTarget[pos + 1] == L'.' && relativeTarget[pos + 2] == L'\\';
while ((startWithDotDotSlash || startWithDotSlash) && lastSeparator != std::string::npos)
{ if (startWithDotSlash)
{
pos += 2;
length -= 2;
} else
{
pos += 3;
length -= 3;
lastSeparator = result.find_last_of(L'\\', lastSeparator - 1); if (processed != nullptr && !processed->empty())
{ if (processed->empty())
{ return false;
}
processed->pop_back();
}
}
startWithDotSlash = length >= 2 && relativeTarget[pos] == L'.' && relativeTarget[pos + 1] == L'\\';
startWithDotDotSlash = length >= 3 && relativeTarget[pos] == L'.' && relativeTarget[pos + 1] == L'.' && relativeTarget[pos + 2] == L'\\';
}
if (lastSeparator == std::string::npos && startWithDotDotSlash)
{ return false;
}
wstring slicedTarget;
slicedTarget.append(relativeTarget, pos, length);
result = result.substr(0, lastSeparator != std::string::npos ? lastSeparator : 0);
if (needToBeProcessed != nullptr)
{ SplitPathsReverse(slicedTarget, *needToBeProcessed);
} else
{
result.push_back(L'\\');
result.append(slicedTarget);
}
return true;}
/// <summary>/// Resolves the reparse points with relative target./// </summary>/// <remarks>/// This method resolves reparse points that occur in the path prefix. This method should only be called when path itself/// is an actionable reparse point whose target is a relative path./// This method traverses each prefix starting from the shortest one. Every time it encounters a directory symlink, it uses GetFinalPathNameByHandle to get the final path./// However, if the prefix itself is a junction, then it leaves the current resolved path intact./// The following example show the needs for this method as a prerequisite in getting/// the immediate target of a reparse point. Suppose that we have the following file system layout://////
repo///
|///
+---intermediate///
|
\---current///
|
symlink1.link ==> ..\..\target\file1.txt///
|
symlink2.link ==> ..\target\file2.txt///
|///
+---source ==> intermediate\current (case 1: directory symlink, case 2: junction)///
|///
\---target///
file1.txt///
file2.txt////// **CASE 1**: source ==> intermediate\current is a directory symlink.////// If a tool accesses repo\source\symlink1.link (say 'type repo\source\symlink1.link'), then the tool should get the content of repo\target\file1.txt./// If the tool accesses repo\source\symlink2.link, then the tool should get path-not-found error because the resolved path will be repo\intermediate\target\file2.txt./// Now, if we try to resolve repo\source\symlink1.link by simply combining it with ..\..\target\file1.txt, then we end up with target\file1.txt (not repo\target\file1.txt),/// which is a non-existent path. To resolve repo\source\symlink1, we need to resolve the reparse points of its prefix, i.e., repo\source. For directory symlinks,/// we need to resolve the prefix to its target. I.e., repo\source is resolved to repo\intermediate\current, and so, given repo\source\symlink1.link, this method returns/// repo\intermediate\current\symlink1.link. Combining repo\intermediate\current\symlink1.link with ..\..\target\file1.txt will give the correct path, i.e., repo\target\file1.txt.////// Similarly, given repo\source\symlink2.link, the method returns repo\intermediate\current\symlink2.link, and combining it with ..\target\file2.txt, will give us/// repo\intermediate\target\file2.txt, which is a non-existent path. This corresponds to the behavior of symlink accesses above.////// **CASE 2**: source ==> intermediate\current is a junction.////// If a tool accesses repo\source\symlink1.link (say 'type repo\source\symlink1.link'), then the tool should get path-not-found error because the resolve path will be target\file1.txt (not repo\target\file1)./// If the tool accesses repo\source\symlink2.link, then the tool should the content of repo\target\file2.txt./// Unlike directory symlinks, when we try to resolve repo\source\symlink2.link, the prefix repo\source is left intact because it is a junction. Thus, combining repo\source\symlink2.link/// with ..\target\file2.txt results in a correct path, i.e., repo\target\file2.txt. The same reasoning can be given for repo\source\symlink1.link, and its resolution results in/// a non-existent path target\file1.txt./// </remarks>static bool TryResolveRelativeTarget(_In_ const wstring& path, _In_ const wstring& relativeTarget, _Inout_ wstring& result, _In_ const PolicyResult& policyResult){
vector<wstring> needToBeProcessed;
vector<wstring> processed;
// Split path into atoms that need to be processed one-by-one. // For example, C:\P1\P2\P3\symlink --> symlink, P3, P1, P2, C: SplitPathsReverse(path, needToBeProcessed);
while (!needToBeProcessed.empty())
{
wstring atom = needToBeProcessed.back();
needToBeProcessed.pop_back();
processed.push_back(atom);
if (!result.empty())
{ // Append directory separator as necessary. if (result[result.length() - 1] != L'\\' && atom[0] != L'\\')
{
result.append(L"\\");
}
}
result.append(atom);
if (needToBeProcessed.empty())
{ // The last atom is the symlink that we are going to replace. break;
}
if (GetReparsePointType(result.c_str(), INVALID_HANDLE_VALUE) == IO_REPARSE_TAG_SYMLINK)
{ // Prefix path is a directory symlink. // For example, C:\P1\P2 is a directory symlink.
// Get the next target of the directory symlink.
wstring target; if (!TryGetReparsePointTarget(result, INVALID_HANDLE_VALUE, target, policyResult))
{ return false;
}
if (GetRootLength(target.c_str()) > 0)
{ // The target of the directory symlink is a rooted path: // - clear result so far, // - restart all the processed atoms, // - initialize the atoms to be processed.
result.clear();
processed.clear(); SplitPathsReverse(target, needToBeProcessed);
} else
{ // The target of the directory symlink is a relative path, then resolve it by "combining" // the directory symlink (stored in the result) and the relative target. if (!TryResolveRelativeTarget(result, target, &processed, &needToBeProcessed))
{ return false;
}
}
}
}
// Finally, resolve the last atom, i.e., the symlink atom. if (!TryResolveRelativeTarget(result, relativeTarget, nullptr, nullptr))
{ return false;
}
return true;}
/// <summary>/// Get the next path of a reparse point path./// </summary>static bool TryGetNextPath(_In_ const wstring& path, _In_ HANDLE hInput, _Inout_ wstring& result, _In_ const PolicyResult& policyResult){
wstring target;
// Get the next target of a reparse point path. if (!TryGetReparsePointTarget(path, hInput, target, policyResult))
{ return false;
}
if (GetRootLength(target.c_str()) > 0)
{ // The next target is a rooted path, then return it as is.
result.assign(target);
} else
{ // The next target is a relative path, then resolve it first. if (!TryResolveRelativeTarget(path, target, result, policyResult))
{ return false;
}
}
return true;}
//////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////// Symlink traversal utilities //////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////
/// <summary>/// Gets chains of the paths leading to and including the final path given the file name./// </summary>static void DetourGetFinalPaths(_In_ const CanonicalizedPath& path, _In_ HANDLE hInput, _Inout_ std::shared_ptr<vector<wstring>>& order, _Inout_ std::shared_ptr<map<wstring, ResolvedPathType, CaseInsensitiveStringLessThan>>& finalPaths, _In_ const PolicyResult& policyResult){
HANDLE handle = hInput;
wstring currentPath = path.GetPathString();
while (true)
{
order->push_back(currentPath);
wstring nextPath; auto nextPathResult = TryGetNextPath(currentPath, handle, nextPath, policyResult);
handle = INVALID_HANDLE_VALUE;
if (nextPathResult)
{ // If there's a next path, then the current path is an intermediate path.
finalPaths->emplace(currentPath, ResolvedPathType::Intermediate);
currentPath = CanonicalizedPath::Canonicalize(nextPath.c_str()).GetPathString();
} else
{ // If the next path was not found, then the current path is considered fully resolved (although full symlink resolution is not enabled here).
finalPaths->emplace(currentPath, ResolvedPathType::FullyResolved); break;
}
if (std::find(order->begin(), order->end(), currentPath) != order->end())
{ // If a cycle was detected in the chain of symlinks, we will log it, and return back the symlinks up to the last resolved path, not including any duplicates. WriteWarningOrErrorF(L"Cycle found when attempting to resolve symlink path '%s'.", path.GetPathString()); break;
}
}}
/// <summary>/// Gets the file attributes for a given path. Returns false if no valid attributes were found or if a NULL path is provided./// </summary>static bool GetFileAttributesByPath(_In_ LPCWSTR lpFileName, _Out_ DWORD& attributes){
DWORD lastError = GetLastError(); if (lpFileName == NULL)
{
attributes = INVALID_FILE_ATTRIBUTES;
} else
{
attributes = GetFileAttributesW(lpFileName);
}
SetLastError(lastError);
return INVALID_FILE_ATTRIBUTES != attributes;}
/// <summary>/// Gets the file attributes for a given handle. Returns false if the GetFileInformationCall fails./// </summary>static bool GetFileAttributesByHandle(_In_ HANDLE hFile, _Out_ DWORD& attributes){
DWORD lastError = GetLastError();
BY_HANDLE_FILE_INFORMATION fileInfo;
BOOL res = GetFileInformationByHandle(hFile, &fileInfo); SetLastError(lastError);
attributes = res ? fileInfo.dwFileAttributes : INVALID_FILE_ATTRIBUTES;
return res;}
static bool ShouldTreatDirectoryReparsePointAsFile(
_In_
DWORD
dwDesiredAccess,
_In_
DWORD
dwFlagsAndAttributes,
_In_
const PolicyResult&
policyResult){ // Directory reparse point is treated as file if // 1. full reparse point resolution is enabled globally or by the access policy, and // 2. the operation performed specifies FILE_FLAG_OPEN_REPARSE_POINT attribute, or the operation is a write operation, and // 3. the policy does not mandate directory symlink to be treated as directory, and // 4. either the operation is not a probe operation, or it is set globally that directory symlink probe should not be treated as directory. // // The first condition of the enablement of full reparse point resolution is required because customers who have not enabled full reparse point resolution // have not specified directory symlinks as files in their spec files. Thus, if those symlinks are treated as files, they will start getting // disallowed file access violations. // // The check for FILE_FLAG_OPEN_REPARSE_POINT is needed to handle operations like CreateFile variants that will access the target directory // if FILE_FLAG_OPEN_REPARSE_POINT is not specified, even though the access is only FILE_READ_ATTRIBUTES. In such a case, the CreateFile call // is often used to probe the existence of the target directory. // // If the operation is a write operation, then the write is done to the directory symlink itself, and not to the target directory, and thus // the directory symlink should be treated as a file. We cannot do the same for read operations, because the read operation could often be used // as a probe operation to check if the target directory exists. Thus, for read operations, we need to check for FILE_FLAG_OPEN_REPARSE_POINT. // // Directory paths specified in the directory translator can be directory symlinks or junctions that are meant to be directories in normal circumstances // Those paths should be marked as being treated as directories in the file access manifest, and thus will be reflected in the policy result. // // If the operation is a probe-only operation, then this is a million dollar question. Ideally, if FILE_FLAG_OPEN_REPARSE_POINT is used, then // the directory symlink should be treated as a directory. However, many Windows tools tend to emit many such innocuous probes through, for example, // FindFirstFile or GetFileAttributes variants. If treated as a file, then the access can be denied (see CheckReadAccess in PolicyResult_common.cpp). // This access denial can break many tools or cause a lot of disallowed file access violations. Thus, we have a global flag whether to treat probed // directory symlinks as a directory or not; for now, the flag is set to true.
return !IgnoreFullReparsePointResolvingForPath(policyResult)
// Full reparse point resolving is enabled,
&& (FlagsAndAttributesContainReparsePointFlag(dwFlagsAndAttributes) // and open attribute contains reparse point flag,
|| WantsWriteAccess(dwDesiredAccess))
//
or write access is requested,
&& !policyResult.TreatDirectorySymlinkAsDirectory()
// and policy does not mandate directory symlink to be treated as directory
&& (!WantsProbeOnlyAccess(dwDesiredAccess)
// and either it is not a probe access,
|| !ProbeDirectorySymlinkAsDirectory());
//
or it is set globally that directory symlink probe should not be treated as directory.}
/// <summary>/// Checks if a path is a directory given a set of attributes. Note that fileOrDirectoryAttribute is not affected by treatReparsePointAsFile./// </summary>static bool IsDirectoryFromAttributes(_In_ DWORD attributes, _In_ bool treatReparsePointAsFile)View remainder of file in raw view
Footer
© 2026 GitHub, Inc.
Footer navigation
Terms
Privacy
Security
Status
Community
Docs
Contact
Manage cookies
Do not share my personal information
You can’t perform that action at this time.